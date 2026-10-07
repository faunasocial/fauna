//! Typed-call wrapper for the user-facing `fauna.conversations.*`
//! WS-RPC kinds — the MLS-channel ciphertext plane clients hit from the
//! conversations page (DM + group chat send / fetch / list).
//!
//! Surface grows as per-cluster slices land (tracked internally). T1b
//! shipped the
//! channel cluster (`channel.{send,fetch,list_for_actor}`); T2 added
//! the keypackage cluster (`keypackage.{upload,fetch,count}`); T3
//! adds same-nest welcome (`welcome.deliver`); T4 adds group creation +
//! send (`group.{create,send_message}`); T5 adds the group action plane
//! (`group.{invite,react,delete}`); T6 adds the group read plane
//! (`group.{list_members,list_messages,list_for_actor}`).
//!
//! Pattern: same shape as `fauna-client-bridges` — a thin
//! `pub struct ConversationsClient { nest: Arc<NestClient> }`, one
//! async method per kind, no state machine. The MLS encrypt / decrypt
//! seam stays in `fauna-mls` (Rust direct on Linux/Windows; via
//! WASM on Web; via UniFFI on Apple/Android) — this crate is the
//! transport surface only.

// Only the native `NestConversationsRpc` (over `Arc<NestClient>`) uses Arc; the
// wasm `WsConversationsRpc` holds the `WsRpcClient` directly. `Weak` breaks the
// session→`NestInboxDrainSource`→session cycle (the session holds the drain
// source) so the receive loop's liveness `Weak` still fails on session drop.
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Weak};

use fauna_client_linkpreview::LinkPreviewClient;
use fauna_client_linkpreview::linkpreview::LinkPreviewResolveReply;
use fauna_protocol::conversations::{
    ChannelActorsRemoteRequest, ChannelActorsReply, ChannelActorsRequest, ChannelFetchReply,
    ChannelFetchRequest, ChannelListForActorReply, ChannelListForActorRequest,
    ChannelSendRemoteRequest, ChannelSendReply, ChannelSendRequest,
    ConversationBlobWriteTokenGetReply, ConversationBlobWriteTokenGetRequest,
    KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET, KeypackageCountReply, KeypackageCountRequest,
    KeypackageFetchReply, KeypackageFetchRequest, KeypackageUploadReply, KeypackageUploadRequest,
    RoomAcceptInviteRemoteRequest, RoomAcceptInviteReply, RoomAcceptInviteRequest,
    RoomBackfillGenerationsReply, RoomBackfillGenerationsRequest, RoomCreateReply,
    RoomCreateRequest, RoomGenerationsRemoteRequest, RoomGenerationsReply, RoomGenerationsRequest,
    RoomInviteRemoteRequest, RoomInviteReply, RoomInviteRequest, RoomLeaveRemoteRequest,
    RoomLeaveReply, RoomLeaveRequest, RoomListInvitesReply, RoomListInvitesRequest,
    RoomListRosterRemoteRequest, RoomListRosterReply, RoomListRosterRequest,
    RoomPublishGenerationReply, RoomPublishGenerationRequest, RoomRemoveReply, RoomRemoveRequest,
    RoomRevokeInviteReply, RoomRevokeInviteRequest, RoomRosterEntryWire,
    RoomRosterReportRemoteRequest, RoomRosterReportReply, RoomRosterReportRequest, RoomSearchReply,
    RoomSearchRequest, RoomSetLabelersReply, RoomSetLabelersRequest, RoomSetPolicyReply,
    RoomSetPolicyRequest, RoomSetReceptionKeyReply, RoomSetReceptionKeyRequest,
    RoomTransferOwnershipReply, RoomTransferOwnershipRequest, WelcomeDeliverReply,
    WelcomeDeliverRequest, WelcomeKind,
};
use fauna_protocol::discovery::{ActorByHandleReply, ActorByHandleRequest};
use fauna_protocol::{RpcErrorAction, RpcErrorClass, RpcRequester};
use serde_bytes::ByteBuf;

// `NestClient` (native reqwest + tokio-tungstenite transport) backs the native
// seam impl + smoke test; not built on wasm (the browser uses `WsRpcClient`).
#[cfg(not(target_arch = "wasm32"))]
use fauna_client::NestClient;
// The public content-addressed download seam `NestMailInboundSource::fetch` uses
// to resolve an over-frame `InboxMessage.body_ref` back to its stored envelope
// (smtp-server.md § Message size limits — the reader half).
#[cfg(not(target_arch = "wasm32"))]
use fauna_client::NestPublicChunkFetcher;
// The MLS replica-plane transport seam both `NestMlsReplicaTransport` (native, over
// `Arc<NestClient>`) and `WsMlsReplicaTransport` (wasm, over `WsRpcClient`) implement —
// on both targets now that the web leg has the wasm twin.
use fauna_client_mls_sync::{MlsReplicaTransport, MlsTransportError, PutOutcome, ReplicaBase};

// The nest-backed inbound-mail read-feed source (`NestMailInboundSource`) — native
// only. The decrypt + key-derivation + calendar-merge crates the source wires.
#[cfg(not(target_arch = "wasm32"))]
use fauna_client_caldav::CalDavClient;
// The shared organizer dispatch-fork seam (`ImipDispatch`) + the conversations
// session whose loaded MLS engine the mailbox-less rail uses — native-only, like
// the scheduling drain. `NestImipDispatch` (below) is the SEND counterpart of
// `NestSchedulingSink`.
#[cfg(not(target_arch = "wasm32"))]
use fauna_client_caldav::ImipDispatch;
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::ConversationsSession;
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::session::FolderWelcomeContext;
// `EmailClient<R>` backs both the shared `NestOutboundMailSink<R>` (both targets)
// and the native-only `NestMailInboundSource`, so it's imported un-cfg'd.
use fauna_client_email::EmailClient;
// `OutboundMailSink` (the send-sink trait) is implemented for `NestOutboundMailSink`
// on both targets; the inbound `*` items below are native-only.
use fauna_conversations::backend::{ConvRpcError, OutboundMailSink, WelcomeChannelKind};
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::backend::{
    InboundMailPage, InboundMailRecord, InboundMailSource, MailFeed, SkippedMailRecord,
};
// This crate's scheduling sink is the native one (`NestSchedulingSink`, over
// `NestClient`); the browser registers its own twin (`libs/fauna-wasm`'s
// `WebSchedulingSink`) on the same crypto-free `SchedulingSink` seam and drives
// the same shared `poll_scheduling_feed` from its JS receive tick. The *seam* and
// the drain are shared — only the two sinks' transports differ.
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::backend::IndexBuilderLauncher;
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::backend::SchedulingSink;
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};
#[cfg(not(target_arch = "wasm32"))]
use tokio_util::sync::CancellationToken;
// The durable inbox-apply backstop seam + the shared drain it runs (layer 3).
// `NestInboxDrainSource` (below) is the missed-push recovery rail, the drain twin
// of `NestSchedulingSink`. Native-only — the browser drives its own JS-timer drain.
// The recipient contact gate is built on both planes (native + wasm), so its
// three inputs are ungated — see `NestFolderGate`.
use fauna_client_contacts::ContactsClient;
#[cfg(not(target_arch = "wasm32"))]
use fauna_client_inbox::{InboxApply, InboxClient};
use fauna_conversations::backend::FolderGateSink;
#[cfg(not(target_arch = "wasm32"))]
use fauna_conversations::backend::InboxDrainSource;
use fauna_core::data::{ArrivalDisposition, ContactStatus, contact_arrival_disposition};
#[cfg(not(target_arch = "wasm32"))]
use fauna_mls::wrapped_blob::{
    SNAPSHOT_GRACE_KEYPAIRS, StandingMailKeypair, derive_mail_epoch_root,
    derive_standing_mail_keypairs,
};
#[cfg(not(target_arch = "wasm32"))]
use zeroize::{Zeroize, ZeroizeOnDrop};
// The on-device INBOX spam scorer wired into `NestMailInboundSource` (native twin
// of the web `WasmConversationsManager`): `MailAccountClient` fetches the sealed
// per-user model + admin-effective policy, `InboxSpamScorer` accumulates the
// verdicts, `BayesianKnobs` carries the effective confidence-ramp/weight knobs
// (mail-spam.md § Scoring placement). Native-only, like the source.
#[cfg(not(target_arch = "wasm32"))]
use fauna_client_bridges::MailAccountClient;
#[cfg(not(target_arch = "wasm32"))]
use fauna_client_mail_settings::InboxSpamScorer;
#[cfg(not(target_arch = "wasm32"))]
use fauna_mail::spam::BayesianKnobs;
#[cfg(not(target_arch = "wasm32"))]
use fauna_protocol::inbox::{SecurityNoticeInbox, WelcomeInbox};

// Cross-nest discovery: the anon hop to the peer nest. Native opens an
// `AnonymousNestClient` (reqwest + tokio-tungstenite); the browser opens its
// wasm twin `AnonymousWsRpcClient` (gloo-net `WebSocket`). Both impl
// `RpcRequester`, so `actor_by_handle_remote` is the same shape on both arms.
#[cfg(not(target_arch = "wasm32"))]
use fauna_anon_client::AnonymousNestClient;
#[cfg(target_arch = "wasm32")]
use fauna_rpc_wasm::AnonymousWsRpcClient;
// Pure domain→base-URL derivation — wasm-clean (no network), so both seam arms
// share it. `resolve_handle_domain` is the anon-discovery dial target;
// `peer_nest_url` is its `Option` face for the relay `nest_url`, and lives
// beside it in `fauna-provisioning` because the contacts knock send derives its
// `recipient_nest_url` from the same rule (priority #4 — one owner, no copies).
use fauna_provisioning::probe::{peer_nest_url, resolve_handle_domain};

/// The bridged rail's nest-backed glue — one implementation on both targets
/// (native over `NestClient` and the mail-key cache, the browser over
/// `WsRpcClient` and the key set its receive loop holds).
mod bridged_glue;
pub use bridged_glue::{
    BridgedKeySource, BridgedKeys, NestBridgedGlue, open_bridged_row, seal_bridged_for_self,
};

/// The content-index builder's advisory `index` lease host. Native-only, like
/// the builder it governs (`content-index.md` § Where queries run — there is no
/// browser tantivy, so the web SPA has no builder to coordinate).
#[cfg(not(target_arch = "wasm32"))]
mod index_lease;
#[cfg(not(target_arch = "wasm32"))]
pub use index_lease::{FIRST_ANSWER_CEILING, IndexLeaseSeat};

/// The custody-ceremony conversations glue (W8.4 (account-data-plane.md § Workstreams)). Native-only for now —
/// web's ceremony wiring rides the web leg.
#[cfg(not(target_arch = "wasm32"))]
mod custody_ceremony_glue;
#[cfg(not(target_arch = "wasm32"))]
pub use custody_ceremony_glue::{
    CustodyCeremonyObserver, CustodyMintCandidate, SessionCustodyPoster, StoreCustodyCeremonySink,
    custody_mint_candidates, mint_candidates_from_channels,
};

/// Test-only tracing capture for the drain-side Welcome-ingest-failure pin — a crate-local copy of
/// `fauna_client_mls_sync::test_tracing`'s `capture_tracing_at_info`, whose
/// own doc says equivalents stay crate-local by design rather than becoming a
/// shared test-support crate.
#[cfg(test)]
mod test_tracing;

pub use fauna_protocol::conversations;

/// Classify a `ConversationsRpc` seam transport error onto the protocol-agnostic
/// [`ConvRpcError`], so the version-mismatch-vs-transient distinction
/// (`version-compatibility.md` Dimension 4) survives the seam into the
/// conversations UI instead of flattening to a raw string (the named `backend.rs`
/// leak). This is the *one* place the seam consumes the shared classifier
/// `RpcError::action()` + renderer `RpcError::localized()` (priority #2 — clients
/// never re-derive "is this retryable?"): a server `RpcError` is mapped by its
/// stable wire `code` (`fauna.nest.outdated` → [`ConvRpcError::NeedsUpdate`]), and
/// a transport fault that never reached the nest (no wire `RpcError`) is a plain
/// retryable [`ConvRpcError::Transient`] carrying the raw error string. Generic
/// over the concrete transport error (native `NestClientError` / wasm
/// `WsRpcError`, both `RpcErrorClass`), so the native and wasm seam arms share it.
fn conv_rpc_error<E: RpcErrorClass + std::fmt::Display>(e: E) -> ConvRpcError {
    if let Some(rpc) = e.as_rpc_error() {
        let message = rpc.localized().to_string();
        return match rpc.action() {
            RpcErrorAction::NeedsUpdate => ConvRpcError::NeedsUpdate { message },
            RpcErrorAction::Rejected => ConvRpcError::Rejected { message },
            RpcErrorAction::Transient => ConvRpcError::Transient { message },
        };
    }
    ConvRpcError::transient(e.to_string())
}

/// A `fauna.conversations.channel.fetch` reply across the protocol-agnostic
/// seam, once for the native and wasm arms alike: the legal-takedown reference
/// (the sealed `envelope` is empty when it is `Some` — the driver renders the
/// tombstone in place of the withheld body) and a community room's verdicts
/// from its home nest (`conversation-rooms.md` § The three classes → *What the
/// home nest does with its read*, purpose 2). The reply's factor rows stay on
/// the wire: no conversation surface composes them yet.
fn fetched_records(reply: ChannelFetchReply) -> Vec<fauna_conversations::backend::FetchedRecord> {
    reply
        .messages
        .into_iter()
        .map(|e| fauna_conversations::backend::FetchedRecord {
            seq: e.seq,
            envelope: e.envelope,
            legal_takedown_ref: e.legal_takedown.map(|m| m.reference),
            labels: e.labels,
            author: e.author,
        })
        .collect()
}

/// The wire code a nest answers `fauna.actor.by_handle` with for an unknown
/// handle (`bins/fauna-nest/src/discovery_handlers.rs`, `DiscoveryError::HandleNotFound`).
const ACTOR_NOT_FOUND_CODE: &str = "fauna.actor.not_found";

/// The wire codes a nest answers `fauna.actor.by_handle` with that genuinely
/// **disown** the handle or the domain — `federation.md` § Peer-auth model →
/// *Discovery-failure semantics* **case 1** ("a nest answered", so the chain may
/// fall through to the email rail). Together with [`ACTOR_NOT_FOUND_CODE`],
/// which the seam answers structurally as `Ok(None)`, this is the **complete**
/// set the nest's discovery handler can refuse a `by_handle` with
/// (`discovery_handlers.rs`: `HandleNotFound` / `fauna.handle.invalid` /
/// `fauna.actor.domain_not_local`).
///
/// ⚠ This list is an **allowlist, and that is the whole point.** The obvious
/// spelling — "`RpcErrorAction::Rejected` means a nest disowned the handle" —
/// is wrong, because `Rejected` is `RpcError::action()`'s **default** arm and
/// therefore an *open* set containing every code the classifier has not been
/// taught. Reading an open set as positive evidence of a definite refusal is
/// what let a throttled probe (`fauna.protocol.rate_limited`) mean "not a Fauna
/// recipient here" and downgrade a **known** Fauna peer to plaintext SMTP.
/// `action()` now classifies that code `Transient`, which fixes the case we
/// know about; this allowlist is what stops the *next* unrecognised code
/// reopening the same hole. An open-ended error class must never be the input
/// to a downgrade decision.
const BY_HANDLE_DISOWNING_CODES: [&str; 2] =
    ["fauna.actor.domain_not_local", "fauna.handle.invalid"];

/// Map the anonymous foreign `fauna.actor.by_handle` reply to the seam's
/// STRUCTURAL outcome — the contract `ConversationsRpc::actor_by_handle_remote`
/// promises and `FaunaMlsBackend::resolve_foreign` decides on
/// (`docs/goal/architecture/federation.md` § Peer-auth model → *Discovery-failure
/// semantics*). Shared by the native and wasm arms so the two cannot drift:
///
/// - the nest answered with the actor → `Ok(Some)`, its `domain` carried
///   through verbatim as [`ResolvedHandle::echoed_domain`] — an assertion the
///   *caller* decides what to do with, which for this foreign arm is "ignore
///   it and name the peer by the domain we dialed";
/// - the nest answered `fauna.actor.not_found` → `Ok(None)` — *not* an error:
///   a nest at this domain disowns the handle, so the chain may fall through;
/// - a wire error in [`BY_HANDLE_DISOWNING_CODES`] → `Rejected`: a nest
///   answered and disowned the handle or the domain (case 1);
/// - any *other* wire error → the shared classifier, with one narrowing: a
///   `Rejected` verdict on a code outside that allowlist is downgraded to
///   `Transient`, because `Rejected` is the classifier's open default arm and
///   "no usable answer" — not "a nest disowned this handle" — is what an
///   unrecognised refusal actually tells this hop. `NeedsUpdate` passes
///   through, and a recognised transient refusal such as a rate limit is
///   already `Transient`;
/// - a transport fault with no wire error → `Transient`, naming the domain.
///
/// Before this, both arms wrapped *every* request error as `Transient` with a
/// raw string and never produced the `Ok(None)` the trait promised, so "no
/// such actor on a live nest" and "no nest answered" were one value — the
/// collapse that let an unreachable peer read as a plain email address.
fn remote_by_handle_outcome<E: RpcErrorClass + std::fmt::Display>(
    domain: String,
    reply: Result<ActorByHandleReply, E>,
) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
    match reply {
        Ok(reply) => Ok(Some(fauna_conversations::backend::ResolvedHandle {
            actor_id_hex: reply.actor_id,
            // Verbatim, and deliberately NOT defaulted to the dialed domain:
            // the field is the answerer's assertion about itself, and
            // `resolve_foreign` builds the canonical handle from the dial it
            // holds anyway. Substituting the dial in here would make an
            // untrustworthy field look authoritative to the next reader.
            echoed_domain: reply.domain,
            addressable: reply.addressable,
        })),
        // The code is cloned out of the borrow before `conv_rpc_error` consumes
        // the error, so the allowlist and the classifier can both see it.
        Err(e) => match e.as_rpc_error().map(|rpc| rpc.code.clone()) {
            Some(code) if code == ACTOR_NOT_FOUND_CODE => Ok(None),
            Some(code) => match conv_rpc_error(e) {
                // See [`BY_HANDLE_DISOWNING_CODES`]: a `Rejected` verdict on a
                // code the classifier does not recognise is the OPEN default
                // arm, not evidence a nest disowned the handle. Hand it to
                // `resolve_foreign` as the non-answer it is, so the known-Fauna-
                // domain rule decides instead of the SMTP rail.
                ConvRpcError::Rejected { message }
                    if !BY_HANDLE_DISOWNING_CODES.contains(&code.as_str()) =>
                {
                    Err(ConvRpcError::Transient { message })
                }
                other => Err(other),
            },
            None => Err(ConvRpcError::transient(format!(
                "by_handle (remote {domain}): {e}"
            ))),
        },
    }
}

/// Resolve a typed `localpart@domain` whose `domain` is **foreign** (not the
/// caller's nest's) directly against that peer nest — the anonymous discovery
/// hop (`docs/goal/architecture/federation.md` § Peer-auth model: discovery is
/// anonymous + TLS, no home-nest relay): resolve the domain to its base URL
/// (`resolve_handle_domain`), open a fresh anon connection (no reconnect
/// supervisor — one per resolve), call `fauna.actor.by_handle`, and hand back
/// the STRUCTURAL outcome (`remote_by_handle_outcome`: found / not-found /
/// refused / no answer). A connect failure is a non-answer.
///
/// One body, two transports: native opens an `AnonymousNestClient`
/// (reqwest + tokio-tungstenite), the browser its wasm twin
/// `AnonymousWsRpcClient` (gloo `WebSocket`). The browser reports a failed
/// WebSocket opaquely — a dead port, a rejected cert and a non-Fauna server
/// all surface as the same `Connect`/`Disconnected` — which is exactly why the
/// backend's discovery rule never consults the transport-error kind.
///
/// Free rather than a method because it needs no home-nest connection at
/// all: the conversations picker reaches it through the
/// [`fauna_conversations::backend::ConversationsRpc`] seam, and the
/// public-folder follow (`fauna_client_folders::follow_ops`) — generic over
/// its home transport — calls it directly (priority #2, one hop).
#[cfg(not(target_arch = "wasm32"))]
pub async fn actor_by_handle_remote(
    domain: &str,
    localpart: &str,
) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
    let target = resolve_handle_domain(domain);
    let client = AnonymousNestClient::connect(&target.base_url)
        .await
        .map_err(|e| ConvRpcError::transient(format!("connect {}: {e}", target.base_url)))?;
    let reply: Result<ActorByHandleReply, _> = client
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: localpart.to_string(),
                domain: None,
                extra: std::collections::BTreeMap::new(),
            },
        )
        .await;
    remote_by_handle_outcome(domain.to_string(), reply)
}

/// The browser arm of [`actor_by_handle_remote`] — see the native one for the
/// contract.
#[cfg(target_arch = "wasm32")]
pub async fn actor_by_handle_remote(
    domain: &str,
    localpart: &str,
) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
    let target = resolve_handle_domain(domain);
    let client = AnonymousWsRpcClient::connect(&target.base_url)
        .map_err(|e| ConvRpcError::transient(format!("connect {}: {e}", target.base_url)))?;
    let reply: Result<ActorByHandleReply, _> = client
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: localpart.to_string(),
                domain: None,
                extra: std::collections::BTreeMap::new(),
            },
        )
        .await;
    remote_by_handle_outcome(domain.to_string(), reply)
}

/// Map a `ConversationsRpc::channel_send` transport error, intercepting the
/// device-owned-epoch commit gate's `fauna.conversations.channel.stale`
/// rejection into the typed [`ConvRpcError::StaleCommit`] rebase signal *before*
/// the generic `Rejected` fallback (`docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync). `latest_commit_seq` is parsed
/// best-effort from the error `details` (the nest embeds `latest_commit_seq=<n>`,
/// `conversations_handlers.rs::stale`); the rebase re-polls from its own
/// processed cursor regardless, so a parse miss is a lost diagnostic, never a
/// correctness issue. Shared by the native + wasm `channel_send` arms.
fn channel_send_error<E: RpcErrorClass + std::fmt::Display>(e: E) -> ConvRpcError {
    if let Some(rpc) = e.as_rpc_error()
        && rpc.code == "fauna.conversations.channel.stale"
    {
        let latest_commit_seq = match rpc.details.as_deref() {
            Some(fauna_protocol::Value::String(s)) => s
                .rsplit("latest_commit_seq=")
                .next()
                .and_then(|n| n.trim().parse::<i64>().ok()),
            _ => None,
        };
        return ConvRpcError::StaleCommit { latest_commit_seq };
    }
    conv_rpc_error(e)
}

/// Map a `fauna.linkpreview.resolve` reply (or transport error) to the protocol-agnostic
/// conversations [`LinkPreviewResolution`](fauna_conversations::backend::LinkPreviewResolution)
/// seam shape (render-model.md § D4) — shared by the native + wasm `LinkPreviewRpc` impls so the
/// reply-mapping is written once (priority #2). A transport error maps to the seam's `ConvRpcError`;
/// the manager collapses both that and an explicit `Failed` to `PreviewState::Failed`.
fn map_link_preview<E: RpcErrorClass + std::fmt::Display>(
    reply: Result<LinkPreviewResolveReply, E>,
) -> Result<fauna_conversations::backend::LinkPreviewResolution, ConvRpcError> {
    use fauna_conversations::backend::LinkPreviewResolution;
    match reply {
        Ok(LinkPreviewResolveReply::Resolved {
            title,
            description,
            image_hash,
        }) => Ok(LinkPreviewResolution::Resolved {
            title,
            description,
            image_hash,
        }),
        // An outcome a newer nest added reads as `Failed`.
        Ok(LinkPreviewResolveReply::Failed | LinkPreviewResolveReply::Unknown) => {
            Ok(LinkPreviewResolution::Failed)
        }
        Err(e) => Err(conv_rpc_error(e)),
    }
}

/// Generic over the transport (`R: RpcRequester`) — same shape as
/// `fauna-client-bridges` / `fauna-client-email` — so one typed-call surface
/// serves the native `Arc<NestClient>` (reqwest + tokio-tungstenite) and the
/// browser `WsRpcClient` (gloo-net over a `WebSocket`). The `R::Error:
/// RpcErrorClass` bound lets [`Self::actor_by_handle`] map the
/// not-found / invalid-handle rejections to `Ok(None)` without naming the
/// concrete transport error.
pub struct ConversationsClient<R: RpcRequester> {
    nest: R,
}

impl<R> ConversationsClient<R>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    pub fn new(nest: R) -> Self {
        Self { nest }
    }

    /// `fauna.conversations.channel.send` — post an MLS ciphertext to
    /// a channel. The nest assigns the per-channel sequence number
    /// (`MAX(seq)+1`) and fans the ciphertext out to subscribers as
    /// `fauna.conversations.channel.message` push events. Replay is
    /// forbidden (`forbid_replay=true`); the auto-retry path won't
    /// re-issue this kind, so the caller must explicitly re-call on
    /// disconnect. 30 s default deadline (see
    /// `fauna_protocol::KindRegistry::register_conversations_channel_kinds`).
    ///
    /// `attachment_refs`: the sealed attachment cids the envelope names — the
    /// conversation kind's blob-reachability floor
    /// (`fauna_conversations::backend::ConversationsRpc::channel_send` owns the
    /// contract); skipped on the wire when empty.
    pub async fn channel_send(
        &self,
        channel_id: impl Into<String>,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<ChannelSendReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.send",
                ChannelSendRequest {
                    channel_id: channel_id.into(),
                    envelope,
                    attachment_refs,
                    // The device-owned-epoch commit precondition: `Some(seq)` for
                    // an MLS commit posted through the rebase discipline (the nest
                    // rejects `fauna.conversations.channel.stale` if a commit
                    // landed since), `None` for a blind application send
                    // (`docs/goal/behavior/devices.md` § Cross-device MLS
                    // group-state sync).
                    expect_no_commit_since,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.channel.send_remote` — the foreign-member send: the
    /// channel's log lives on `home_nest_url` (recorded from the Welcome), so
    /// the caller's own nest relays the envelope there via
    /// `fauna.federation.channel.append` (`direct-messages.md` § step 3b). A
    /// distinct kind by the ratified wire rule — an old, relay-unaware nest
    /// fails loud (`unknown_kind`) instead of silently appending to its local
    /// log (the send blackhole).
    pub async fn channel_send_remote(
        &self,
        channel_id: impl Into<String>,
        home_nest_url: impl Into<String>,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<ChannelSendReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.send_remote",
                ChannelSendRemoteRequest {
                    channel_id: channel_id.into(),
                    nest_url: home_nest_url.into(),
                    envelope,
                    expect_no_commit_since,
                    attachment_refs,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.blob.write_token.get` — a **foreign member** asks
    /// its own nest to relay a short-lived, write-only bulk-byte token from the
    /// channel's home nest, so its sealed attachment bytes can be POSTed DIRECT
    /// to that home nest's `POST /api/v1/blob` (the blob twin of
    /// [`Self::channel_send_remote`]; `conversation-rooms.md` § The home nest →
    /// *Attachment bytes*). Returns `(token, expires_at)` — `expires_at` is
    /// absolute Unix seconds. Always a relay: `home_nest_url` is required.
    pub async fn blob_write_token_get(
        &self,
        channel_id: impl Into<String>,
        home_nest_url: impl Into<String>,
    ) -> Result<ConversationBlobWriteTokenGetReply, R::Error> {
        self.nest
            .request(
                KIND_CONVERSATIONS_BLOB_WRITE_TOKEN_GET,
                ConversationBlobWriteTokenGetRequest {
                    channel_id: channel_id.into(),
                    nest_url: home_nest_url.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.channel.fetch` — pull messages from a
    /// channel with `seq > after`, clamped to `[1, 500]` server-side.
    /// Polling fallback for push-missed messages; replay-safe at 5 s.
    ///
    /// `home_nest_url` is `Some(url)` when the channel's log lives on a **foreign**
    /// nest (the home nest relays via `fauna.federation.channel.fetch`), `None` for
    /// same-nest — mirroring `keypackage_fetch` / `welcome_deliver`'s `nest_url`.
    pub async fn channel_fetch(
        &self,
        channel_id: impl Into<String>,
        after: i64,
        limit: i64,
        home_nest_url: Option<String>,
    ) -> Result<ChannelFetchReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.fetch",
                ChannelFetchRequest {
                    channel_id: channel_id.into(),
                    after,
                    limit,
                    nest_url: home_nest_url,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.channel.list_for_actor` — list the hex
    /// channel IDs the calling actor is registered on. Pure read;
    /// replay-safe at 5 s. (The HTTP twin's `?actor_id=…` path param
    /// is implicit on the WS-RPC plane — the caller is the connection's
    /// actor.)
    pub async fn channel_list_for_actor(&self) -> Result<ChannelListForActorReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.list_for_actor",
                ChannelListForActorRequest {
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.channel.actors` — the inverse read: which hex
    /// actor ids sit on one channel's routing roster. Member-scoped (the nest
    /// refuses a caller who is not themselves on the channel). Pure read;
    /// replay-safe at 5 s.
    pub async fn channel_actors(
        &self,
        channel_id_hex: impl Into<String>,
    ) -> Result<ChannelActorsReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.actors",
                ChannelActorsRequest {
                    channel_id: channel_id_hex.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.channel.actors_remote` — the same roster read for
    /// a **foreign-homed** channel: the caller's own nest relays it to the
    /// channel's home nest via `fauna.federation.channel.actors` and answers
    /// the home's authoritative union. A distinct kind — an old nest fails
    /// loud (`unknown_kind`) instead of answering its partial roster as a
    /// clean success (`federation.md` § Cross-nest).
    pub async fn channel_actors_remote(
        &self,
        channel_id_hex: impl Into<String>,
        home_nest_url: impl Into<String>,
    ) -> Result<ChannelActorsReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.channel.actors_remote",
                ChannelActorsRemoteRequest {
                    channel_id: channel_id_hex.into(),
                    nest_url: home_nest_url.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.keypackage.upload` — publish one or more
    /// MLS key packages on behalf of the calling actor. The caller is
    /// implicit (the connection's actor); the HTTP twin's
    /// path-param-vs-bearer match is implicit on the WS-RPC plane.
    /// Replay-safe at 5 s — a duplicate upload wastes storage
    /// (30-day expiry) but FIFO consumption tolerates it.
    ///
    /// `last_resort` mirrors the wire field 1:1: `false` for the consumable
    /// one-time pool (the login top-up), `true` for the single reusable
    /// last-resort key package published at onboarding (Spec Y2 slice 3 —
    /// `docs/goal/architecture/federation.md` § Key packages). The nest keeps a
    /// single last-resort row per actor (the upload replaces any prior one), so
    /// re-publishing on every login is idempotent.
    pub async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<KeypackageUploadReply, R::Error> {
        let packages = packages.into_iter().map(ByteBuf::from).collect();
        self.nest
            .request(
                "fauna.conversations.keypackage.upload",
                KeypackageUploadRequest {
                    packages,
                    last_resort,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.keypackage.fetch` — consume (FIFO oldest
    /// non-expired) one key package belonging to `target_actor_id` (hex
    /// `[u8; 32]`). `nest_url` is `None` for a same-nest fetch; `Some(url)`
    /// names a foreign peer nest, and the home nest signs + relays the fetch to
    /// it (Spec Y2 cross-nest relay, `federation.md` § Federation residue).
    /// `forbid_replay=true` (the auto-retry path won't re-issue) — the
    /// caller must explicitly re-call on disconnect. 5 s default deadline.
    /// `reply.key_package == None` ⇔ no non-expired KP available.
    pub async fn keypackage_fetch(
        &self,
        target_actor_id: impl Into<String>,
        nest_url: Option<String>,
    ) -> Result<KeypackageFetchReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.keypackage.fetch",
                KeypackageFetchRequest {
                    actor_id: target_actor_id.into(),
                    nest_url,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.keypackage.count` — non-destructive count
    /// of remaining non-expired key packages for `target_actor_id`.
    /// Pure read; replay-safe at 5 s. Used by senders to gauge whether
    /// `keypackage.fetch` will succeed before initiating a chat.
    pub async fn keypackage_count(
        &self,
        target_actor_id: impl Into<String>,
    ) -> Result<KeypackageCountReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.keypackage.count",
                KeypackageCountRequest {
                    actor_id: target_actor_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.actor.by_handle` — resolve a bare handle (localpart, no domain)
    /// to its actor on this nest. A pre-identity discovery kind, but also
    /// routable on the authenticated connection (the router is shared); the
    /// recipient picker calls it to promote a typed handle to a Fauna address.
    /// Maps the `fauna.actor.not_found` / `fauna.handle.invalid` rejections to
    /// `Ok(None)` (the handle simply doesn't resolve here) so the caller's
    /// resolution chain falls through; every other failure is `Err`. Pure read.
    pub async fn actor_by_handle(
        &self,
        handle: impl Into<String>,
    ) -> Result<Option<ActorByHandleReply>, R::Error> {
        match self
            .nest
            .request::<_, ActorByHandleReply>(
                "fauna.actor.by_handle",
                ActorByHandleRequest {
                    handle: handle.into(),
                    domain: None,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
        {
            Ok(reply) => Ok(Some(reply)),
            // Map the "handle doesn't resolve here" rejections to `Ok(None)` so
            // the caller's resolution chain falls through; `as_rpc_error()`
            // exposes the wire code generically (native `NestClientError` / wasm
            // `WsRpcError` both impl `RpcErrorClass`), so this stays
            // transport-agnostic. Every other failure is a real `Err`.
            Err(e)
                if e.as_rpc_error().is_some_and(|rpc| {
                    fauna_protocol::discovery::is_unresolved_handle_code(&rpc.code)
                }) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// `fauna.conversations.welcome.deliver` — same-nest MLS Welcome
    /// delivery (`recipient_actor_id` lives on this nest). Pushes a row
    /// into the recipient's inbox AND fires a
    /// `fauna.conversations.welcome.received` push event AND best-effort
    /// APNS/FCM. `forbid_replay=true` (the auto-retry path won't
    /// re-issue) — the caller must explicitly re-issue on disconnect;
    /// replay would duplicate inbox rows and re-fire mobile pushes.
    /// 30 s default deadline.
    ///
    /// `nest_url` is `None` for same-nest delivery; `Some(url)` names the
    /// recipient's foreign peer nest and the home nest signs + relays the
    /// Welcome to it (Spec Y2 cross-nest relay, `federation.md`
    /// § Federation residue).
    pub async fn welcome_deliver(
        &self,
        recipient_actor_id: impl Into<String>,
        channel_id: impl Into<String>,
        welcome_bytes: Vec<u8>,
        kind: WelcomeKind,
        nest_url: Option<String>,
    ) -> Result<WelcomeDeliverReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.welcome.deliver",
                WelcomeDeliverRequest {
                    recipient_actor_id: recipient_actor_id.into(),
                    channel_id: channel_id.into(),
                    welcome_bytes,
                    kind,
                    nest_url,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.roster_report` — report a room's membership
    /// to its home nest, the **member-reported floor roster**
    /// (`conversation-rooms.md` § The floor roster → *End-to-end rooms*).
    ///
    /// Called after every membership commit this device authors on a governed
    /// room. The nest replaces the room's roster wholesale and reads it for
    /// routing fan-out, the custody serve door, the relay gate and succession
    /// targets — never for confidentiality, which stays MLS's.
    ///
    /// The nest admits the report from a live member of the roster it is
    /// replacing (the ratchet), so an add, a remove and a departing member's
    /// final report all pass while a departed member cannot report itself
    /// back in.
    ///
    /// `commit_seq` is the log position of the commit the roster follows;
    /// the nest does not apply a report older than the one its floor holds
    /// (the reply's `superseded_by` says so) — see
    /// [`RoomRosterReportRequest::commit_seq`].
    pub async fn room_roster_report(
        &self,
        room_id: impl Into<String>,
        members: Vec<RoomRosterEntryWire>,
        policy_version: Option<u64>,
        commit_seq: Option<i64>,
    ) -> Result<RoomRosterReportReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.roster_report",
                RoomRosterReportRequest {
                    room_id: room_id.into(),
                    members,
                    policy_version,
                    commit_seq,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.list_roster` — read a room's floor roster
    /// from its home nest, the **read half** of
    /// [`Self::room_roster_report`] (`conversation-rooms.md` § The floor
    /// roster). Admitted only to a live member of the room it names.
    ///
    /// The reply's per-principal `handle`/`domain` are **joined nest-side**
    /// from the nest's own `users` rows, never taken from the member report
    /// that wrote the roster — which is the whole reason this read, rather
    /// than a richer report, is what resolves a member's name. `None` for a
    /// principal homed on another nest.
    ///
    /// `at_policy_version` asks the room to also serve the signed policy it
    /// held at that version, and its birth salt
    /// ([`RoomListRosterRequest::at_policy_version`]); `None` is the plain read.
    pub async fn room_list_roster(
        &self,
        room_id: impl Into<String>,
        at_policy_version: Option<u64>,
    ) -> Result<RoomListRosterReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.list_roster",
                RoomListRosterRequest {
                    room_id: room_id.into(),
                    at_policy_version,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.list_roster_remote` — the same roster read
    /// for a room homed on **another nest**: this nest relays
    /// `fauna.federation.conversation.roster.fetch` to `nest_url` and hands
    /// back the room home's answer unchanged (`conversation-rooms.md` § The
    /// home nest).
    ///
    /// A distinct kind rather than an additive `nest_url` on
    /// [`Self::room_list_roster`] — see [`RoomListRosterRemoteRequest`] for
    /// why an old own-nest's clean `permission_denied` is the degradation that
    /// must fail loud instead.
    ///
    /// `at_policy_version` is forwarded to the room's home unchanged, as on
    /// [`Self::room_list_roster`].
    pub async fn room_list_roster_remote(
        &self,
        room_id: impl Into<String>,
        nest_url: impl Into<String>,
        at_policy_version: Option<u64>,
    ) -> Result<RoomListRosterReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.list_roster_remote",
                RoomListRosterRemoteRequest {
                    room_id: room_id.into(),
                    nest_url: nest_url.into(),
                    at_policy_version,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.roster_report_remote` — the same roster
    /// report for a room homed on **another nest**: this nest relays
    /// `fauna.federation.conversation.roster.report` to `nest_url` and hands
    /// back the room home's ack unchanged (`conversation-rooms.md` § The home
    /// nest). The write twin of [`Self::room_list_roster_remote`].
    ///
    /// A distinct kind rather than an additive `nest_url` on
    /// [`Self::room_roster_report`] — see [`RoomRosterReportRemoteRequest`]
    /// for why an old own-nest's stray-floor bootstrap is the degradation that
    /// must fail loud instead.
    pub async fn room_roster_report_remote(
        &self,
        room_id: impl Into<String>,
        nest_url: impl Into<String>,
        members: Vec<RoomRosterEntryWire>,
        policy_version: Option<u64>,
        commit_seq: Option<i64>,
    ) -> Result<RoomRosterReportReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.roster_report_remote",
                RoomRosterReportRemoteRequest {
                    room_id: room_id.into(),
                    nest_url: nest_url.into(),
                    members,
                    policy_version,
                    commit_seq,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.create` — the community class's **birth
    /// ceremony** (`conversation-rooms.md` § Implementation status today).
    ///
    /// The request names no class, no home and no member list: all three are
    /// consequences. The nest re-derives the room id from the birth record
    /// (`salt` plus the owner the signed policy names), seats the creator as
    /// the room's one owner and itself as an ordinary member, and the class
    /// derives as `community` from that member set.
    ///
    /// `reception_pubkey` is the creator's group-reception public half — the
    /// wrap target its founding roster row carries, so the room's first mint
    /// has somebody to wrap to.
    pub async fn room_create(
        &self,
        salt: impl Into<String>,
        policy: Vec<u8>,
        reception_pubkey: Vec<u8>,
    ) -> Result<RoomCreateReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.create",
                RoomCreateRequest {
                    salt: salt.into(),
                    policy,
                    reception_pubkey,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.invite` — the inviter's signed act
    /// (`conversation-rooms.md` § Join rules and invites).
    ///
    /// `invite` is a canonical dag-cbor
    /// `fauna_mls::room_policy::SignedRoomInvite`; the nest binds its signer
    /// to this authenticated caller, checks the caller's rank against the
    /// room's join rule, and gates the invitee's own **reach policy** exactly
    /// as it gates a group Welcome. `invitee_node` is the invitee's home nest
    /// as the inviter knows it — empty means this nest.
    ///
    /// **An invitation does not seat anybody**; acceptance does. (The retired
    /// group plane's invite seated the member outright; the room plane
    /// deliberately does not.)
    pub async fn room_invite(
        &self,
        invite: Vec<u8>,
        invitee_node: impl Into<String>,
    ) -> Result<RoomInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.invite",
                RoomInviteRequest {
                    invite,
                    invitee_node: invitee_node.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.invite_remote` — the same invitation for a
    /// room homed on **another nest**, issued by a member homed here: this
    /// nest relays `fauna.federation.conversation.room.invite_issue` to
    /// `nest_url` and hands back the room home's ack unchanged
    /// (`conversation-rooms.md` § Join rules and invites → *A cross-nest
    /// invitation*, the foreign-inviter leg). The issuing twin of
    /// [`Self::room_accept_invite_remote`].
    ///
    /// A distinct kind rather than an additive `nest_url` on
    /// [`Self::room_invite`] — see [`RoomInviteRemoteRequest`] for why an old
    /// own-nest's "no such room" is the degradation that must fail loud
    /// instead. `invitee_node` keeps its meaning: empty for an invitee homed
    /// on this account's own nest, which the room's home resolves itself.
    pub async fn room_invite_remote(
        &self,
        invite: Vec<u8>,
        nest_url: impl Into<String>,
        invitee_node: impl Into<String>,
    ) -> Result<RoomInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.invite_remote",
                RoomInviteRemoteRequest {
                    invite,
                    nest_url: nest_url.into(),
                    invitee_node: invitee_node.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.accept_invite` — the act that **seats** the
    /// caller on the room's floor.
    ///
    /// Only the invitee accepts, and only its own pending invitation: the
    /// caller is the key, so there is nothing to name and nothing to spoof.
    /// `reception_pubkey` is this account's wrap target, carried here because
    /// the roster row and the wrap target are one fact — a member cannot be
    /// seated without the room knowing how to key it.
    pub async fn room_accept_invite(
        &self,
        room_id: impl Into<String>,
        reception_pubkey: Vec<u8>,
    ) -> Result<RoomAcceptInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.accept_invite",
                RoomAcceptInviteRequest {
                    room_id: room_id.into(),
                    reception_pubkey,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.accept_invite_remote` — the same acceptance
    /// for a room homed on **another nest**: this nest relays
    /// `fauna.federation.conversation.room.accept` to `nest_url` and hands
    /// back the room home's ack unchanged (`conversation-rooms.md` § Join
    /// rules and invites → *A cross-nest invitation*). The seating twin of
    /// [`Self::room_leave_remote`].
    ///
    /// A distinct kind rather than an additive `nest_url` on
    /// [`Self::room_accept_invite`] — see [`RoomAcceptInviteRemoteRequest`]
    /// for why an old own-nest's "no invitation pending" is the degradation
    /// that must fail loud instead.
    pub async fn room_accept_invite_remote(
        &self,
        room_id: impl Into<String>,
        nest_url: impl Into<String>,
        reception_pubkey: Vec<u8>,
    ) -> Result<RoomAcceptInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.accept_invite_remote",
                RoomAcceptInviteRemoteRequest {
                    room_id: room_id.into(),
                    nest_url: nest_url.into(),
                    reception_pubkey,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.list_invites` — the invitations pending on a
    /// community room that this caller may withdraw
    /// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
    /// are visible to whoever may withdraw them*). The nest scopes the reply:
    /// everything for the owner and admins, what it issued for any other
    /// seated member; a caller off the floor is refused.
    pub async fn room_list_invites(
        &self,
        room_id: impl Into<String>,
    ) -> Result<RoomListInvitesReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.list_invites",
                RoomListInvitesRequest {
                    room_id: room_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.revoke_invite` — withdraw the invitation
    /// pending for `invitee`: the row, its standing envelope and the
    /// envelope's quota charge, as one act. The invitee is told nothing, and
    /// withdrawing what is not pending is answered `revoked: false`, not
    /// refused.
    pub async fn room_revoke_invite(
        &self,
        room_id: impl Into<String>,
        invitee: impl Into<String>,
    ) -> Result<RoomRevokeInviteReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.revoke_invite",
                RoomRevokeInviteRequest {
                    room_id: room_id.into(),
                    invitee: invitee.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.backfill_generations` — cover a newly-seated
    /// member with the generations its room's history policy allows.
    ///
    /// The **add** half of the scheme's mint triggers: an add never mints, it
    /// wraps what already exists to the new entry, so this door can never move
    /// the room's tip. `wraps` are canonical dag-cbor
    /// `fauna_core::group_generation::GroupTopupRecord` values, verified as a
    /// batch — a refused batch leaves no partial history slice behind.
    pub async fn room_backfill_generations(
        &self,
        room_id: impl Into<String>,
        target_actor_id: impl Into<String>,
        wraps: Vec<Vec<u8>>,
    ) -> Result<RoomBackfillGenerationsReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.backfill_generations",
                RoomBackfillGenerationsRequest {
                    room_id: room_id.into(),
                    target_actor_id: target_actor_id.into(),
                    wraps,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.set_reception_key` — supply, or rotate, the
    /// wrap target of the seat this caller already holds
    /// (`community-rooms.md` § Implementation status today, *A seat gains or
    /// rotates its wrap target*). The seat keeps its entry; a key already set
    /// is replaced; the reply names the tip the seat holds no wrap for, when
    /// there is one.
    pub async fn room_set_reception_key(
        &self,
        room_id: impl Into<String>,
        reception_pubkey: Vec<u8>,
    ) -> Result<RoomSetReceptionKeyReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.set_reception_key",
                RoomSetReceptionKeyRequest {
                    room_id: room_id.into(),
                    reception_pubkey,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.publish_generation` — the members' **keying
    /// act**, which the nest admits and never performs
    /// (`conversation-rooms.md` § The three classes → *Community*).
    ///
    /// `mint` is a canonical dag-cbor
    /// `fauna_core::group_generation::GroupGenerationMintRecord::Minted`. The
    /// nest admits it on three checks and no more: the minter is this
    /// authenticated caller and holds owner or admin on the floor, the mint
    /// names the room's current tip as its one parent, and its wraps cover
    /// every live floor principal that has a wrap target.
    pub async fn room_publish_generation(
        &self,
        room_id: impl Into<String>,
        mint: Vec<u8>,
    ) -> Result<RoomPublishGenerationReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.publish_generation",
                RoomPublishGenerationRequest {
                    room_id: room_id.into(),
                    mint,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.remove` — unseat another principal from the
    /// room's floor (`conversation-rooms.md` § Roles and authorization: an
    /// owner's and an admin's, never a member's). Answers the live floor count.
    ///
    /// It only unseats. **The severance is the ROTATION that follows** — a
    /// removed member who can still fetch ciphertext through a relay reads
    /// nothing new only once a generation is minted without it (§ The three
    /// classes → *Community*, reason 4) — which is a separate act on a separate
    /// door, because the nest never mints.
    pub async fn room_remove(
        &self,
        room_id: impl Into<String>,
        principal: impl Into<String>,
    ) -> Result<RoomRemoveReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.remove",
                RoomRemoveRequest {
                    room_id: room_id.into(),
                    principal: principal.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.leave` — unseat **yourself**. Answers the live
    /// floor count.
    ///
    /// Its own door rather than a self-addressed remove because the
    /// authorization differs in kind: removing someone else is a rank, leaving
    /// is a right every member but the owner has (an owner transfers first — a
    /// room is never owner-less).
    pub async fn room_leave(&self, room_id: impl Into<String>) -> Result<RoomLeaveReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.leave",
                RoomLeaveRequest {
                    room_id: room_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.leave_remote` — the same departure for a room
    /// homed on **another nest**: this nest relays
    /// `fauna.federation.conversation.room.leave` to `nest_url` and hands back
    /// the room home's ack unchanged (`conversation-rooms.md` § The home nest).
    /// The self-scoped twin of [`Self::room_roster_report_remote`].
    ///
    /// A distinct kind rather than an additive `nest_url` on
    /// [`Self::room_leave`] — see [`RoomLeaveRemoteRequest`] for why an old
    /// own-nest's "no such room" is the degradation that must fail loud
    /// instead, and for why the leaver is left seated without it.
    pub async fn room_leave_remote(
        &self,
        room_id: impl Into<String>,
        nest_url: impl Into<String>,
    ) -> Result<RoomLeaveReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.leave_remote",
                RoomLeaveRemoteRequest {
                    room_id: room_id.into(),
                    nest_url: nest_url.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.set_policy` — store a replacement policy at
    /// `stored + 1`. Answers the version now stored.
    ///
    /// The nest stores it and cannot author it: it checks that the signature
    /// covers the change and that the version is a strict ratchet. Ownership is
    /// refused here by name — it moves through
    /// [`Self::room_transfer_ownership`].
    pub async fn room_set_policy(
        &self,
        room_id: impl Into<String>,
        policy: Vec<u8>,
    ) -> Result<RoomSetPolicyReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.set_policy",
                RoomSetPolicyRequest {
                    room_id: room_id.into(),
                    policy,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.set_labelers` — store a community room's
    /// replacement labeler set at `stored + 1`. Answers the version now
    /// stored. The policy's sibling record: owner or admin signs, the nest
    /// stores it and cannot author it.
    pub async fn room_set_labelers(
        &self,
        room_id: impl Into<String>,
        labelers: Vec<u8>,
    ) -> Result<RoomSetLabelersReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.set_labelers",
                RoomSetLabelersRequest {
                    room_id: room_id.into(),
                    labelers,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.transfer_ownership` — hand the room to another
    /// live user member. `policy` is signed by the **outgoing** owner, whose
    /// role in the previous version is what lets it change the owner field.
    pub async fn room_transfer_ownership(
        &self,
        room_id: impl Into<String>,
        policy: Vec<u8>,
    ) -> Result<RoomTransferOwnershipReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.transfer_ownership",
                RoomTransferOwnershipRequest {
                    room_id: room_id.into(),
                    policy,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.generations` — read back the room's
    /// generations, each carrying **this caller's own wrap** and nothing else
    /// (`conversation-rooms.md` § The three classes → *Community*). How a
    /// member gets its keys, and how a joiner gets the retained bundle the
    /// history policy allows.
    ///
    /// The door cannot enumerate a room's key material: another member's wraps
    /// are never served, so what comes back is exactly what this caller could
    /// already open.
    pub async fn room_generations(
        &self,
        room_id: impl Into<String>,
    ) -> Result<RoomGenerationsReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.generations",
                RoomGenerationsRequest {
                    room_id: room_id.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.generations_remote` — the same read for a
    /// room homed on **another nest**, relayed by this caller's own nest
    /// (`conversation-rooms.md` § The home nest).
    ///
    /// A distinct kind for the [`Self::room_list_roster_remote`] reason,
    /// sharpened: an old own-nest that ignored an additive `nest_url` would
    /// answer from its own empty room plane as a clean success, and the caller
    /// would read "this room has no generations" — a silent wrong answer about
    /// **key material**, which is the one subject where a loud unknown-kind
    /// failure is unambiguously the better outcome.
    pub async fn room_generations_remote(
        &self,
        room_id: impl Into<String>,
        nest_url: impl Into<String>,
    ) -> Result<RoomGenerationsReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.generations_remote",
                RoomGenerationsRemoteRequest {
                    room_id: room_id.into(),
                    nest_url: nest_url.into(),
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }

    /// `fauna.conversations.room.search` — search a community room through
    /// the derived view its home nest built from the sealed log
    /// (`conversation-rooms.md` § The three classes → *What the home nest
    /// does with its read*). Admitted to a live floor member of any rank.
    ///
    /// This is the one search a member cannot run for itself: the community
    /// class is the unbounded one, and a device holds only the slice of the
    /// log it has fetched. Every other class searches client-side and this
    /// door refuses it.
    ///
    /// ⚠ **The hits name log positions, never text.** The nest indexes under
    /// the tip generation while a member holds wraps only for the
    /// generations minted while it sat on the floor, so a snippet here would
    /// be the nest handing over plaintext the sealing plane withholds. Take
    /// `hit.seq`, read the message back through the channel, and open it
    /// with a wrap you hold — a `seq` you cannot open is a message you were
    /// not keyed for, which is the answer, not a failure.
    ///
    /// `include_posts` also asks for the room's **room-restricted posts** the
    /// home nest stores (`hit.kind == Post`, named by `hit.post_id`, read back
    /// through `fauna.posts.get` and opened with the room's key — the same
    /// where-never-what rule). Only a caller that renders post hits sets it;
    /// an older nest ignores it and answers messages only.
    pub async fn room_search(
        &self,
        room_id: impl Into<String>,
        query: impl Into<String>,
        limit: Option<u32>,
        include_posts: bool,
    ) -> Result<RoomSearchReply, R::Error> {
        self.nest
            .request(
                "fauna.conversations.room.search",
                RoomSearchRequest {
                    room_id: room_id.into(),
                    query: query.into(),
                    limit,
                    include_posts,
                    extra: std::collections::BTreeMap::new(),
                },
            )
            .await
    }
}

// ── Outbound-mail send sink (`NestOutboundMailSink`) ───────────────────────────

/// The canonical outbound-mail send sink — the **send** twin of
/// [`NestMailInboundSource`], shared by every app that drives the conversations
/// SMTP rail: linux (Rust-direct), the native UniFFI apps via `fauna-ffi`'s
/// `conversations_session` factory (macOS / iOS / Windows / Android), and the
/// browser SPA via `fauna-wasm`. Forwards a composed RFC 5322 message over
/// `fauna.email.send` through the shared [`EmailClient`] (generic over the
/// transport `R: RpcRequester` — native `Arc<NestClient>`, wasm `WsRpcClient`),
/// surfacing any per-recipient relay error the nest reported. The crypto-free
/// transport glue the transport-free `fauna_conversations::SmtpBackend` injects
/// (priority #2 — no per-app mail logic beyond this submit call). Wrap in `Arc`
/// and hand to `ConversationsSession::register_smtp` / `SmtpBackend::new`.
///
/// **Why a generic struct + two concrete trait shims (not one generic impl):**
/// [`OutboundMailSink`] is dual-armed (`Send` futures off wasm, `?Send` on wasm),
/// and `RpcRequester::request`'s future carries **no** `Send` bound — it's inferred
/// per concrete transport (see [`fauna_protocol::RpcRequester`]). So the trait impl
/// can't be written once over a generic `R` (the native `#[async_trait]` arm could
/// not prove the boxed future `Send` for an arbitrary `R`). The shared send logic
/// therefore lives in the inherent generic [`Self::submit_inner`]; the two concrete
/// trait shims below — native `Arc<NestClient>`, wasm `WsRpcClient` — each delegate
/// to it (Send-ness inferred per concrete transport).
pub struct NestOutboundMailSink<R: RpcRequester> {
    email: EmailClient<R>,
}

impl<R: RpcRequester> NestOutboundMailSink<R> {
    /// Build the send sink over `nest` — the same transport handle the rest of the
    /// conversations rails ride.
    pub fn new(nest: R) -> Self {
        Self {
            email: EmailClient::new(nest),
        }
    }

    /// Submit a composed RFC 5322 message over `fauna.email.send`: map a transport
    /// error to its `Display`, then surface any per-recipient relay error the nest
    /// reported (the local delivery, if any, may still have succeeded). The single
    /// source of the send logic both trait shims delegate to — a plain inherent
    /// `async fn` so its future's `Send`-ness is inferred per concrete `R` (the
    /// native shim boxes a `Send` future, the wasm shim a `!Send` one).
    async fn submit_inner(
        &self,
        recipients: Vec<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), String> {
        let reply = self
            .email
            .send(recipients, raw_rfc5322)
            .await
            .map_err(|e| e.to_string())?;
        if !reply.remote_errors.is_empty() {
            return Err(reply.remote_errors.join("; "));
        }
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl OutboundMailSink for NestOutboundMailSink<Arc<NestClient>> {
    async fn submit(&self, recipients: Vec<String>, raw_rfc5322: Vec<u8>) -> Result<(), String> {
        self.submit_inner(recipients, raw_rfc5322).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl OutboundMailSink for NestOutboundMailSink<fauna_rpc_wasm::WsRpcClient> {
    async fn submit(&self, recipients: Vec<String>, raw_rfc5322: Vec<u8>) -> Result<(), String> {
        self.submit_inner(recipients, raw_rfc5322).await
    }
}

/// Native `ConversationsRpc` seam impl — drives `FaunaMlsBackend`'s nest I/O
/// over the typed [`ConversationsClient`].
///
/// `fauna_conversations::backend::ConversationsRpc` is the object-safe,
/// **protocol-agnostic** seam (raw bytes + hex ids) `FaunaMlsBackend` uses for
/// all nest I/O, so `fauna-conversations` itself stays free of `fauna-protocol`.
/// This adapter is the one place the seam meets the typed `fauna.conversations.*`
/// kinds. Shared by every native app (Rust-direct on Linux/Windows, UniFFI on
/// Apple/Android) — registered once per client via
/// `FaunaMlsBackend::new(engine, Arc::new(NestConversationsRpc::new(nest)), …)`.
///
/// `cfg(not(target_arch = "wasm32"))`: the native arm uses `#[async_trait]`
/// (`Send` futures from `NestClient`). The browser SPA registers the parallel
/// [`WsConversationsRpc`] (below) over the `!Send` `WsRpcClient` — both impl the
/// same dual-armed seam (Track E2a unified the trait's `Send` bound).
#[cfg(not(target_arch = "wasm32"))]
pub struct NestConversationsRpc {
    client: ConversationsClient<Arc<NestClient>>,
    /// The raw client too — the conversation-attachment blob put/get ride the
    /// nest's content-addressed byte-source surface over HTTP (`/api/v1/blob`),
    /// not WS-RPC, so they need the `NestClient`'s authed `reqwest` channel
    /// directly (`ConversationsClient` only exposes the typed WS-RPC kinds).
    nest: Arc<NestClient>,
    /// The HTTP client for a **foreign** home nest's byte plane — a room homed
    /// elsewhere keeps its attachment bytes there (`conversation-rooms.md` § The
    /// home nest → *Attachment bytes*), and this member holds no session and no
    /// SPKI pin for that nest. Plain WebPKI, exactly the posture of
    /// [`fauna_client::ForeignPublicChunkFetcher`] for a cross-nest shared
    /// folder: the bytes are sealed and integrity rests on the content address.
    /// Injectable ([`Self::with_foreign_http`]) for tests whose in-process
    /// "foreign nest" serves a self-signed floor cert.
    foreign_http: reqwest::Client,
    /// One cached write-token bearer per `(home_nest_url, channel)` — minted via
    /// `fauna.conversations.blob.write_token.get` on this member's OWN nest
    /// (relayed to the home nest behind its foreign-member gate), refreshed
    /// proactively and on a 401 by [`fauna_client::write_token_bearer::WriteTokenBearer`],
    /// the same cache the shared-folder writer and the segment-backup
    /// coordinator ride.
    foreign_bearers: std::sync::Mutex<
        std::collections::HashMap<
            (String, String),
            Arc<fauna_client::write_token_bearer::WriteTokenBearer>,
        >,
    >,
    /// The **classified verdict** of the most recent write-token mint that
    /// failed, per `(home_nest_url, channel)` — the one thing the byte plane
    /// cannot carry for us.
    ///
    /// A mint failure reaches [`Self::blob_put`] as the bearer's
    /// [`ApiError`](fauna_nest_http::ApiError), propagated verbatim by
    /// `ReqwestNestContentApi::send`'s `self.bearer.bearer().await?`. But an
    /// *upload* refusal arrives as an `ApiError::Status { code: 403, .. }` too,
    /// carrying the home nest's raw body text — so no shape of `ApiError` can
    /// tell the two apart, and mapping every 403 to a refusal would put raw
    /// server English in `BackendError::Refusal`, which renders it verbatim
    /// (`conversations.md` § Errors & edge cases forbids exactly that).
    ///
    /// Widening the shared [`WriteTokenBearer`](fauna_client::write_token_bearer::WriteTokenBearer)
    /// to carry the verdict was the alternative and was rejected: it is
    /// deliberately `ApiError`-typed across three planes — the shared-folder
    /// writer, the segment-backup coordinator and this one — and its module doc
    /// states the split ("each caller owns only its mint call and its error
    /// classification"). So the verdict is kept here, on the one caller that
    /// needs it, instead of in everyone's type.
    ///
    /// Scoped exactly like the bearer it shadows, taken (not cloned) on read, and
    /// cleared on a successful mint, so a stale refusal cannot be attributed to a
    /// later upload.
    foreign_mint_verdicts:
        Arc<std::sync::Mutex<std::collections::HashMap<(String, String), ConvRpcError>>>,
}

/// Classify a cross-nest attachment **mint** failure once, for both planes that
/// have to know about it, so they cannot disagree.
///
/// Two consumers, two vocabularies, one verdict:
/// - the **byte plane** takes an [`ApiError`](fauna_nest_http::ApiError), whose
///   contract ([`fauna_client::write_token_bearer::WriteTokenBearer::from_minter`])
///   asks a refusal to be a `403` so the transfer fails hard instead of retrying
///   a refusal as if it were a network blip;
/// - the **send seam** takes a [`ConvRpcError`], which decides whether the user
///   is told to retry and what sentence they read.
///
/// The verdict itself comes from the shared [`conv_rpc_error`] — i.e. from
/// `RpcError::action()` and `RpcError::localized()`, never from matching on the
/// `NestClientError` variant. That distinction is the whole finding: matching the variant collapsed two opposite faults into one bucket.
/// A membership refusal (`fauna.federation.forbidden` from the home nest's
/// federation gate) is permanent and became a retryable `Transient` one frame
/// up, while an unreachable home nest — which our own nest reports as
/// `internal("federation mint failed")`, a wire `RpcError` and the canonical
/// *retryable* fault — became a hard `403`. Both were wrong, in opposite
/// directions, for the same reason.
///
/// `NeedsUpdate` joins `Rejected` on the byte-plane side: a nest too old to know
/// the mint kind will not learn it by being asked again.
///
/// The folder plane's `fauna_sync_engine::write_token_bearer::classify_mint_error`
/// is the reference for why the byte-plane split is load-bearing; it keys off the
/// narrower `is_access_revoked` because it also has a park-the-engine side effect
/// to trigger, which this plane has not.
#[cfg(not(target_arch = "wasm32"))]
fn classify_blob_mint_error<E: RpcErrorClass + std::fmt::Display>(
    e: E,
) -> (fauna_nest_http::ApiError, ConvRpcError) {
    let conv = conv_rpc_error(e);
    let api = match &conv {
        // Permanent: the home nest will not authorize this write, and asking
        // again cannot change that. `message` is already the localized sentence.
        ConvRpcError::Rejected { message } | ConvRpcError::NeedsUpdate { message } => {
            fauna_nest_http::ApiError::Status {
                code: 403,
                message: message.clone(),
            }
        }
        // Retryable: keep the transport framing the byte plane already treats as
        // a soft failure.
        ConvRpcError::Transient { message } => {
            fauna_nest_http::ApiError::Transport(message.clone())
        }
        // `StaleCommit` is the commit-gate's rebase signal and cannot arise from
        // a write-token mint; treat it as retryable rather than inventing a
        // refusal, and let the seam-side verdict carry the detail.
        ConvRpcError::StaleCommit { .. } => fauna_nest_http::ApiError::Transport(conv.to_string()),
    };
    (api, conv)
}

/// The send-slot verdict for a failed attachment UPLOAD with no mint verdict to
/// prefer — shared by the native and wasm `blob_put`, which is what keeps web
/// from leaking what native no longer does.
///
/// **Classified by who answered, not by which step failed.** The upload goes to
/// our own nest, or — for a room homed elsewhere — straight to that room's home
/// nest, which is chosen by whoever created the room: on an invitation, a
/// stranger. `e`'s text is that responder's own words (`HTTP {code}: {body}`),
/// and `Transient`'s payload reaches `BackendError::user_detail()` verbatim
/// (`conversations.md` § Errors & edge cases). So a foreign responder's words
/// are never the sentence: the user reads one localized product statement, and
/// the words go to the log line beside the sealed cid, exactly as the mint arm
/// does. Our own nest keeps today's behaviour — its transport sentence is what
/// the taxonomy lets `Transient` carry.
///
/// The bucket does not change on either arm: an upload failure stays retryable.
/// Only the words the user reads do.
fn classify_blob_upload_error(foreign: bool, e: impl std::fmt::Display) -> ConvRpcError {
    if foreign {
        ConvRpcError::foreign_attachment_upload_failed()
    } else {
        ConvRpcError::transient(e.to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NestConversationsRpc {
    pub fn new(nest: Arc<NestClient>) -> Self {
        Self::with_foreign_http(nest, reqwest::Client::new())
    }

    /// [`Self::new`] with a caller-supplied HTTP client for **foreign** home
    /// nests' byte plane — for tests whose in-process foreign nest serves a
    /// self-signed floor cert (production keeps the default WebPKI client: a
    /// room's home nest is a real reachable deployment with a valid cert).
    pub fn with_foreign_http(nest: Arc<NestClient>, foreign_http: reqwest::Client) -> Self {
        Self {
            client: ConversationsClient::new(nest.clone()),
            nest,
            foreign_http,
            foreign_bearers: std::sync::Mutex::new(std::collections::HashMap::new()),
            foreign_mint_verdicts: Arc::new(
                std::sync::Mutex::new(std::collections::HashMap::new()),
            ),
        }
    }

    /// The cached write-token bearer for `(home_nest_url, channel)`, minting
    /// on first use through this member's own nest. A typed refusal from the
    /// relay (not a member of that channel on its home nest, or a home nest too
    /// old to know the mint kind) is a `403` — the transfer fails hard rather
    /// than retrying a refusal as if it were a network blip — while a genuine
    /// transport fault keeps its retryable `Transport` framing (the folder
    /// plane's `classify_mint_error` distinction, one rail over).
    fn foreign_write_bearer(
        &self,
        home_nest_url: &str,
        channel_id_hex: &str,
    ) -> Arc<fauna_client::write_token_bearer::WriteTokenBearer> {
        let key = (home_nest_url.to_string(), channel_id_hex.to_string());
        let mut cache = self.foreign_bearers.lock().unwrap();
        if let Some(b) = cache.get(&key) {
            return Arc::clone(b);
        }
        let own_nest = Arc::clone(&self.nest);
        let (home, channel) = key.clone();
        // The minter records its classified verdict here so `blob_put` can tell a
        // mint refusal from an upload refusal — see `foreign_mint_verdicts`.
        let verdicts = Arc::clone(&self.foreign_mint_verdicts);
        let verdict_key = key.clone();
        let bearer = Arc::new(
            fauna_client::write_token_bearer::WriteTokenBearer::from_minter(move || {
                let own_nest = Arc::clone(&own_nest);
                let home = home.clone();
                let channel = channel.clone();
                let verdicts = Arc::clone(&verdicts);
                let verdict_key = verdict_key.clone();
                async move {
                    match ConversationsClient::new(own_nest)
                        .blob_write_token_get(channel, home)
                        .await
                    {
                        Ok(reply) => {
                            // A mint that worked retires any earlier refusal, so a
                            // later upload failure is never blamed on it.
                            verdicts.lock().unwrap().remove(&verdict_key);
                            Ok((reply.token, reply.expires_at))
                        }
                        Err(e) => {
                            // One classification, by `action()`, for both planes.
                            let (api, conv) = classify_blob_mint_error(e);
                            verdicts.lock().unwrap().insert(verdict_key, conv);
                            Err(api)
                        }
                    }
                }
            }),
        );
        cache.insert(key, Arc::clone(&bearer));
        bearer
    }
}

// The home-nest link-preview seam (render-model.md § D4) — `ConversationsManager` holds this
// directly (it is rail-agnostic, unlike `ConversationsRpc`), wired with the SAME object the glue
// builds for the FaunaMls backend. Over `Arc<NestClient>`'s WS-RPC requester.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::LinkPreviewRpc for NestConversationsRpc {
    async fn link_preview_resolve(
        &self,
        url: String,
    ) -> Result<fauna_conversations::backend::LinkPreviewResolution, ConvRpcError> {
        map_link_preview(LinkPreviewClient::new(self.nest.clone()).resolve(url).await)
    }
}

/// Project a [`RoomRosterReport`] onto the wire and send it, for both the
/// native and wasm arms.
///
/// **Answers `Undelivered` on every failure rather than propagating**, because
/// the caller's contract is best-effort-and-tallied: the membership commit
/// that owed this report is already on the log, so a failed report costs
/// routing and custody-serving precision, never confidentiality and never the
/// gesture that owed it (`FaunaMlsBackend::report_roster`). A failed report
/// (a network failure, a nest restart) lands there, which is the honest
/// reading: the report went unconfirmed.
///
/// A delivered report is `Stored` or `Superseded` by the ack's
/// `superseded_by`, which is a **third** answer and not a shade of either:
/// the home nest understood the report and declined to apply it because its
/// floor is at least as new. Collapsing it into success is what left a device
/// unable to tell that its own report was never the one applied.
///
/// A `None` role is carried as absent rather than defaulted — the difference
/// between a policy-less room (no roles at all) and a governed one is the whole
/// distinction the nest stores, and inventing `member` here would erase it.
async fn report_roster<R>(
    client: &ConversationsClient<R>,
    report: fauna_conversations::backend::RoomRosterReport,
) -> fauna_conversations::backend::RoomRosterReportOutcome
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let fauna_conversations::backend::RoomRosterReport {
        channel_hex,
        members,
        policy_version,
        commit_seq,
        home_nest_url,
    } = report;
    let members = members
        .into_iter()
        .map(|m| RoomRosterEntryWire {
            actor: m.actor.to_hex(),
            role: m.role.map(|r| room_role_wire(r).to_string()),
            extra: Default::default(),
        })
        .collect();
    // The kind pick lives HERE, once, for both glue arms — the `read_roster`
    // shape: a foreign-homed room rides the distinct relay kind
    // `room.roster_report_remote`, which the reporter's own nest originates on
    // to the room's home; a same-nest room stays on the plain report. Both
    // answer the same ack, because the relay forwards the room home's reply
    // unchanged.
    // Both arms carry the commit's log position: it is what the room's home
    // orders reports by, and a relay that dropped it would leave that home's
    // floor rollback-prone for every foreign member.
    use fauna_conversations::backend::RoomRosterReportOutcome;
    let sent = match home_nest_url {
        Some(url) => {
            client
                .room_roster_report_remote(channel_hex, url, members, policy_version, commit_seq)
                .await
        }
        None => {
            client
                .room_roster_report(channel_hex, members, policy_version, commit_seq)
                .await
        }
    };
    match sent {
        // The ack's `superseded_by` is carried back rather than dropped: it is
        // the only signal a reporting device gets that its own report was not
        // the one the floor applied (`conversation-rooms.md` § The floor
        // roster).
        Ok(reply) => match reply.superseded_by {
            None => RoomRosterReportOutcome::Stored,
            by => RoomRosterReportOutcome::Superseded { by },
        },
        Err(e) => {
            tracing::debug!("floor-roster report not delivered: {e}");
            RoomRosterReportOutcome::Undelivered
        }
    }
}

/// The three-role vocabulary's wire spelling
/// (`conversation-rooms.md` § Roles and authorization). Exhaustive on purpose:
/// a new role must be spelled here deliberately, not defaulted into an
/// existing one.
fn room_role_wire(role: fauna_conversations::room::RoomRole) -> &'static str {
    use fauna_conversations::room::RoomRole;
    match role {
        RoomRole::Owner => "owner",
        RoomRole::Admin => "admin",
        RoomRole::Member => "member",
    }
}

/// Read a room's floor roster and project it onto
/// [`fauna_conversations::backend::RoomRosterKnownMember`], for both the
/// native and wasm arms — the read twin of [`report_roster`].
///
/// **Distinguishes a clean refusal from a failed one** (): the door
/// answers `fauna.conversations.permission_denied` for both "no floor exists"
/// and "you are not a member of it" — [`fauna_conversations::backend::RoomRosterRead::NoFloor`]
/// either way, since they degrade identically for every caller but the
/// backfill. Every other rejection — a transport fault, a nest restart — is
/// [`fauna_conversations::backend::RoomRosterRead::Unavailable`]: it did not
/// say "no", it simply failed to answer. Most callers still don't care
/// ([`fauna_conversations::backend::RoomRosterRead::or_absent`] collapses
/// both to `None`, "this member stays elided", which is what this seam
/// already did before the split).
///
/// **Every principal is carried, and nothing is filtered here.** This read
/// used to drop everything but `user`, which was right while its only consumer
/// resolved handles — and wrong the moment a second one appeared: the class is
/// a pure function of the member set (`conversation-rooms.md` § The three
/// classes, TP8), so a caller that never sees the nest row cannot tell a
/// community room from an end-to-end one, and a *minter* that never sees it
/// cannot grant the home nest the read the class exists for. The user-only
/// narrowing now lives with the consumer that wants it
/// (`FaunaMlsBackend::resolve_nameless_members`), which is one line there and
/// was a silent data loss here.
///
/// A member whose `principal` is not 64-hex is still dropped rather than
/// defaulted — an actor id that does not parse names nobody, and inventing one
/// here would attach a handle to the wrong row. An `entry_id` that is not
/// 32 bytes is carried as absent for the reason
/// [`read_generations`] drops such a row outright: a wrap binds to the entry
/// id as AAD, so a truncated one is not a slot, and `None` is what the
/// coverage rule already calls *unkeyable*.
async fn read_roster<R>(
    client: &ConversationsClient<R>,
    channel_hex: String,
    home_nest_url: Option<String>,
) -> fauna_conversations::backend::RoomRosterRead
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    // The kind pick lives HERE, once (the `channel_actors` precedent): a
    // foreign-homed room rides the distinct relay kind
    // `room.list_roster_remote`, which the caller's own nest originates on to
    // the room's home; a same-nest room stays on the plain read. Both answer
    // the same `RoomListRosterReply`, because the relay forwards the room
    // home's reply unchanged.
    let read = match home_nest_url {
        Some(url) => client.room_list_roster_remote(channel_hex, url, None).await,
        None => client.room_list_roster(channel_hex, None).await,
    };
    let reply = match read {
        Ok(reply) => reply,
        // "not a member of this room" / "no such room" both answer this exact
        // code (`bins/fauna-nest/src/conversations_handlers.rs`'s
        // `permission_denied`, forwarded unchanged through the relay by
        // `map_peer_relay_error`) — a clean, confirmed refusal, not a failed
        // read. Anything else — disconnect, timeout — did not say "no", it just didn't answer.
        Err(e) => {
            return if e
                .as_rpc_error()
                .is_some_and(|rpc| rpc.code == "fauna.conversations.permission_denied")
            {
                tracing::debug!(
                    "floor-roster read: no floor, or not a member — members stay elided"
                );
                fauna_conversations::backend::RoomRosterRead::NoFloor
            } else {
                tracing::debug!(error = %e, "floor-roster read failed — members stay elided");
                fauna_conversations::backend::RoomRosterRead::Unavailable
            };
        }
    };
    fauna_conversations::backend::RoomRosterRead::Floor(fauna_conversations::backend::RoomFloor {
        members: reply
            .members
            .into_iter()
            .filter_map(|m| {
                let actor = fauna_core::identity::ActorId::from_hex(&m.principal).ok()?;
                Some(fauna_conversations::backend::RoomRosterKnownMember {
                    actor,
                    handle: m.handle,
                    domain: m.domain,
                    kind: fauna_conversations::backend::RoomPrincipalKind::from_wire(&m.kind),
                    role: m.role.as_deref().and_then(room_role_of_wire),
                    entry_id: m.entry_id.as_deref().and_then(hex32),
                    reception_pubkey: m.reception_pubkey,
                    joined_at_ms: m.joined_at,
                    tip_wrapped: m.tip_wrapped,
                })
            })
            .collect(),
        policy_version: reply.policy_version,
        // Carried through opaque: this layer holds no MLS vocabulary, and the
        // signature is checked at the reader that acts on the fields.
        policy: reply.policy.map(|p| p.into_vec()),
        // Opaque for the same reason — the backend verifies the set, and that
        // it names this room, before a name of it renders.
        labelers: reply.labelers.map(|l| l.into_vec()),
    })
}

/// The versioned roster read behind
/// [`fauna_conversations::backend::RoomRosterReader::read_policy_version`],
/// shared by both targets' glue objects (the [`read_roster`] shape, and its
/// kind pick).
///
/// Only the two fields the anchor wants leave this function, and both leave it
/// **unverified** — this layer holds no MLS vocabulary, and the chain is judged
/// by the backend that acts on it. A refusal and an answer without the version
/// are both `NotHeld` (final for the act that asked); anything that did not
/// reach an answer is `Unavailable`, which the backend retries.
async fn read_policy_version<R>(
    client: &ConversationsClient<R>,
    channel_hex: String,
    home_nest_url: Option<String>,
    version: u64,
) -> fauna_conversations::backend::RoomPolicyVersionRead
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    use fauna_conversations::backend::RoomPolicyVersionRead;
    let read = match home_nest_url {
        Some(url) => {
            client
                .room_list_roster_remote(channel_hex, url, Some(version))
                .await
        }
        None => client.room_list_roster(channel_hex, Some(version)).await,
    };
    match read {
        Ok(reply) => match reply.policy_at_version {
            Some(policy) => RoomPolicyVersionRead::Served {
                policy: policy.into_vec(),
                birth_salt: reply
                    .birth_salt
                    .and_then(|salt| <[u8; 32]>::try_from(salt.as_slice()).ok()),
            },
            None => RoomPolicyVersionRead::NotHeld,
        },
        Err(e)
            if e.as_rpc_error()
                .is_some_and(|rpc| rpc.code == "fauna.conversations.permission_denied") =>
        {
            RoomPolicyVersionRead::NotHeld
        }
        Err(e) => {
            tracing::debug!(error = %e, "policy-version read failed — the record stays unpainted");
            RoomPolicyVersionRead::Unavailable
        }
    }
}

/// The wire's role word as this crate's role, or `None` for a policy-less room's
/// absent role and for a word this build does not know.
///
/// The inverse of [`room_role_wire`], and deliberately **not** total: an
/// unrecognised rank must not decode as `member`, since a rank is what every
/// gate on both sides reads. Unknown reads as no role at all, which renders as
/// a policy-less room does — honest about what this build cannot judge.
fn room_role_of_wire(word: &str) -> Option<fauna_conversations::room::RoomRole> {
    use fauna_conversations::room::RoomRole;
    match word {
        "owner" => Some(RoomRole::Owner),
        "admin" => Some(RoomRole::Admin),
        "member" => Some(RoomRole::Member),
        _ => None,
    }
}

/// The generation read behind [`fauna_conversations::backend::RoomGenerationReader`],
/// shared by both targets' glue objects (the [`read_roster`] shape).
///
/// The kind pick lives here, once, and for the sharper version of the roster
/// read's reason: a foreign-homed room rides the distinct relay kind
/// `room.generations_remote`, because an old own-nest that ignored an additive
/// `nest_url` would answer "no generations" as a clean success — a silent
/// wrong answer about key material.
///
/// A failure degrades to `None`, which the backend reads as "this room's
/// sealed records stay unopened this pass" and retries on the next poll — a
/// nest that was down must not be written off for the session.
///
/// A row whose ids are not 32 bytes is **dropped, not defaulted**: a wrap is
/// bound to `(generation_id, entry_id)` as AAD, so a padded or truncated id
/// would turn a refusal that means "this is not your wrap" into one that means
/// nothing at all.
async fn read_generations<R>(
    client: &ConversationsClient<R>,
    channel_hex: String,
    home_nest_url: Option<String>,
) -> Option<Vec<fauna_conversations::backend::RoomGenerationWrap>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let read = match home_nest_url {
        Some(url) => client.room_generations_remote(channel_hex, url).await,
        None => client.room_generations(channel_hex).await,
    };
    let reply = match read {
        Ok(reply) => reply,
        Err(e) => {
            tracing::debug!(error = %e, "room generation read failed — sealed records stay unopened");
            return None;
        }
    };
    Some(
        reply
            .generations
            .into_iter()
            .filter_map(|g| {
                Some(fauna_conversations::backend::RoomGenerationWrap {
                    generation_id: hex32(&g.generation_id)?,
                    key_commitment: <[u8; 32]>::try_from(g.key_commitment.as_slice()).ok()?,
                    wrap: g.wrap,
                    entry_id: hex32(&g.entry_id)?,
                    is_tip: g.is_tip,
                })
            })
            .collect(),
    )
}

/// A 64-hex string as 32 bytes, or `None` — the wire's id form.
fn hex32(s: &str) -> Option<[u8; 32]> {
    hex::decode(s).ok()?.try_into().ok()
}

/// The birth ceremony behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects (the [`read_roster`] shape).
///
/// **Errors propagate here**, unlike every other room read in this file. A
/// failed roster read leaves a member elided and a failed generation read
/// leaves a record unopened — both recoverable on the next poll — whereas a
/// founding that quietly failed would leave the caller holding a thread bound
/// to a room that does not exist.
///
/// Same-nest only, and that is the door's own shape rather than an omission:
/// a room is born on its **creating member's** home nest
/// (`conversation-rooms.md` § The home nest), so there is no other nest for
/// this call to reach.
async fn create_room<R>(
    client: &ConversationsClient<R>,
    salt_hex: String,
    policy: Vec<u8>,
    reception_pubkey: Vec<u8>,
) -> Result<String, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_create(salt_hex, policy, reception_pubkey)
        .await
        .map(|reply| reply.room_id)
        .map_err(conv_rpc_error)
}

/// The invitation behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects. Errors propagate for
/// [`create_room`]'s reason.
///
/// `invitee_node` arrives as the chip's **handle domain** (what the backend
/// knows about a foreign invitee) and leaves as the **base URL** the room's
/// home nest dials to deliver the invitation — the same `peer_nest_url`
/// derivation the Welcome relay's `nest_url` takes, so a nest is addressed one
/// way on both ceremonies. An empty domain stays empty: the invitee is homed
/// on this account's own nest.
///
/// `home_nest_url` is the room's recorded home — `Some(url)` when the room is
/// homed on another nest, and then the invitation rides the distinct relay
/// kind ([`ConversationsClient::room_invite_remote`]) rather than an additive
/// field, for the reason `RoomInviteRemoteRequest` states. The same pick
/// [`leave_room`] makes off the same signal. The node keeps its meaning on
/// both kinds: the room's home resolves an empty node to the relaying nest.
async fn invite_to_room<R>(
    client: &ConversationsClient<R>,
    invite: Vec<u8>,
    invitee_node: String,
    home_nest_url: Option<String>,
) -> Result<String, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let invitee_node =
        peer_nest_url(Some(invitee_node).filter(|d| !d.is_empty())).unwrap_or_default();
    let reply = match home_nest_url.filter(|u| !u.is_empty()) {
        Some(url) => client.room_invite_remote(invite, url, invitee_node).await,
        None => client.room_invite(invite, invitee_node).await,
    };
    reply.map(|reply| reply.role).map_err(conv_rpc_error)
}

/// The acceptance behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects.
///
/// `home_nest_url` is the invitation's `room_node` — `Some(url)` when the room
/// is homed on another nest, and then the acceptance rides the distinct relay
/// kind ([`ConversationsClient::room_accept_invite_remote`]) rather than an
/// additive field, for the reason `RoomAcceptInviteRemoteRequest` states. The
/// same pick [`leave_room`] makes off the same signal.
async fn accept_room_invite<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    reception_pubkey: Vec<u8>,
    home_nest_url: Option<String>,
) -> Result<String, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply = match home_nest_url.filter(|u| !u.is_empty()) {
        Some(url) => {
            client
                .room_accept_invite_remote(room_id_hex, url, reception_pubkey)
                .await
        }
        None => {
            client
                .room_accept_invite(room_id_hex, reception_pubkey)
                .await
        }
    };
    reply.map(|reply| reply.role).map_err(conv_rpc_error)
}

/// The room-side invitation list behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects. A row whose ids or role do not parse is dropped rather than
/// rendered half-named — the [`read_roster`] rule.
async fn list_room_invites<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
) -> Result<Vec<fauna_conversations::backend::PendingRoomInvite>, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply = client
        .room_list_invites(room_id_hex)
        .await
        .map_err(conv_rpc_error)?;
    Ok(reply
        .invites
        .into_iter()
        .filter_map(|i| {
            let qualified = |handle: Option<String>| {
                let handle = handle.filter(|h| !h.is_empty())?;
                Some(match i.domain.as_deref().filter(|d| !d.is_empty()) {
                    Some(domain) => format!("{handle}@{domain}"),
                    None => handle,
                })
            };
            Some(fauna_conversations::backend::PendingRoomInvite {
                invitee: fauna_core::identity::ActorId::from_hex(&i.invitee).ok()?,
                inviter: fauna_core::identity::ActorId::from_hex(&i.inviter).ok()?,
                role: room_role_of_wire(&i.role)?,
                invitee_handle: qualified(i.invitee_handle),
                inviter_handle: qualified(i.inviter_handle),
                invited_at_ms: i.invited_at,
                still_acceptable: i.still_acceptable,
            })
        })
        .collect())
}

/// The withdrawal behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects. Answers whether an invitation was
/// consumed; `false` is the nest's answer, not an error.
async fn revoke_room_invite<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    invitee_hex: String,
) -> Result<bool, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_revoke_invite(room_id_hex, invitee_hex)
        .await
        .map(|reply| reply.revoked)
        .map_err(conv_rpc_error)
}

/// The invitation **read** behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects.
///
/// The room plane has no read kind of its own for this, by design: an invitation
/// is "delivered to the invitee's home nest through the inbox plane"
/// (`conversation-rooms.md` § Join rules and invites), so this reads the generic
/// inbox every app already drains and translates it into room vocabulary — the
/// one place in the fleet that knows an invitation arrives as an
/// `InboxKind::RoomInvite`. It is a **peek**: the walk acks nothing, so polling
/// it consumes no invitation.
///
/// The bytes stay opaque here. Verifying them is the *reader's* job
/// (`FaunaMlsBackend::pending_room_invitations`), which is where the MLS
/// vocabulary and the decision to trust a name both live.
async fn pending_room_invitations<R>(
    requester: R,
) -> Result<Vec<fauna_conversations::backend::PendingRoomInvitation>, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let client = fauna_client_inbox::InboxClient::new(requester);
    let invites = fauna_client_inbox::list_pending_room_invites(&client, 0)
        .await
        .map_err(conv_rpc_error)?;
    Ok(invites
        .into_iter()
        .map(|i| fauna_conversations::backend::PendingRoomInvitation {
            id: i.inbox_id,
            signed_invite: i.signed_invite,
            room_node: i.room_node,
        })
        .collect())
}

/// The invitation **settle** behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects — the `fauna.inbox.ack` that consumes the delivered knock once
/// the user has accepted or declined it.
async fn settle_room_invitation<R>(requester: R, id: i64) -> Result<(), ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    fauna_client_inbox::InboxClient::new(requester)
        .ack(vec![id])
        .await
        .map(|_| ())
        .map_err(conv_rpc_error)
}

/// The removal behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects. It unseats and nothing more — the
/// severance rotation is the caller's second act.
async fn remove_from_room<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    principal_hex: String,
) -> Result<u32, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_remove(room_id_hex, principal_hex)
        .await
        .map(|reply| reply.members)
        .map_err(conv_rpc_error)
}

/// The departure behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects.
///
/// The kind pick lives HERE, once — the [`read_roster`] shape: a foreign-homed
/// room rides the distinct relay kind `room.leave_remote`, which the leaver's
/// own nest originates on to the room's home; a same-nest room stays on the
/// plain leave. Both answer the same live floor count, because the relay
/// forwards the room home's reply unchanged.
///
/// The pick is not an optimization: a room's floor lives on its home nest
/// alone, so the same-nest door aimed at a room homed elsewhere answers "no
/// such room" and the member stays seated — with its seat still drawing every
/// later generation's wrap and still refusing its own re-admission.
async fn leave_room<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    home_nest_url: Option<String>,
) -> Result<u32, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let left = match home_nest_url {
        Some(url) => client.room_leave_remote(room_id_hex, url).await,
        None => client.room_leave(room_id_hex).await,
    };
    left.map(|reply| reply.members).map_err(conv_rpc_error)
}

/// The policy replacement behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects.
async fn set_room_policy<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    policy: Vec<u8>,
) -> Result<u64, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_set_policy(room_id_hex, policy)
        .await
        .map(|reply| reply.policy_version)
        .map_err(conv_rpc_error)
}

/// The labeler-set replacement behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects.
async fn set_room_labelers<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    labelers: Vec<u8>,
) -> Result<u64, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_set_labelers(room_id_hex, labelers)
        .await
        .map(|reply| reply.labelers_version)
        .map_err(conv_rpc_error)
}

/// The hand-over behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects.
async fn transfer_room_ownership<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    policy: Vec<u8>,
) -> Result<u64, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_transfer_ownership(room_id_hex, policy)
        .await
        .map(|reply| reply.policy_version)
        .map_err(conv_rpc_error)
}

/// The add-side backfill behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects.
async fn backfill_room_generations<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    target_actor_id_hex: String,
    wraps: Vec<Vec<u8>>,
) -> Result<(), ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_backfill_generations(room_id_hex, target_actor_id_hex, wraps)
        .await
        .map(|_| ())
        .map_err(conv_rpc_error)
}

/// The seat's own wrap target behind
/// [`fauna_conversations::backend::RoomCeremonyRpc`], shared by both targets'
/// glue objects. Same-nest for [`publish_room_generation`]'s reason: the
/// seat it binds is a row of the floor, which lives on the room's home alone.
async fn set_room_reception_key<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    reception_pubkey: Vec<u8>,
) -> Result<fauna_conversations::backend::RoomReceptionKeyBound, ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply = client
        .room_set_reception_key(room_id_hex, reception_pubkey)
        .await
        .map_err(conv_rpc_error)?;
    let hex32 = |field: &str, s: &str| -> Result<[u8; 32], ConvRpcError> {
        hex::decode(s)
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())
            .ok_or_else(|| ConvRpcError::Rejected {
                message: format!("the nest answered a malformed {field}: {s}"),
            })
    };
    Ok(fauna_conversations::backend::RoomReceptionKeyBound {
        entry_id: hex32("entry_id", &reply.entry_id)?,
        rotated: reply.rotated,
        uncovered_tip: reply
            .uncovered_tip
            .as_deref()
            .map(|t| hex32("uncovered_tip", t))
            .transpose()?,
    })
}

/// The keying act behind [`fauna_conversations::backend::RoomCeremonyRpc`],
/// shared by both targets' glue objects.
///
/// Same-nest only for the sharper version of [`create_room`]'s reason: a
/// mint is admitted against the floor roster and the generation tip, both of
/// which live on the room's home nest alone. A member homed elsewhere reaches
/// them through a relay kind that does not exist yet, which is why an app that
/// mints today is one seated on the room's own home.
async fn publish_room_generation<R>(
    client: &ConversationsClient<R>,
    room_id_hex: String,
    mint: Vec<u8>,
) -> Result<(), ConvRpcError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    client
        .room_publish_generation(room_id_hex, mint)
        .await
        .map(|_| ())
        .map_err(conv_rpc_error)
}

// The floor-roster report seam (`conversation-rooms.md` § The floor roster) —
// a SEPARATE trait on the same glue object, exactly like `LinkPreviewRpc`
// above and deliberately not a method on `ConversationsRpc`: ten types
// implement that trait, eight of them test doubles that have no room plane
// and no business growing one. `FaunaMlsBackend` holds this behind its own
// `set_room_roster_reporter` seam, so the object the glue already builds for
// the backend is the object that reports.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::RoomRosterReporter for NestConversationsRpc {
    async fn report(
        &self,
        report: fauna_conversations::backend::RoomRosterReport,
    ) -> fauna_conversations::backend::RoomRosterReportOutcome {
        report_roster(&self.client, report).await
    }
}

// The read half, on the same object for the same reason.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::RoomRosterReader for NestConversationsRpc {
    async fn read_roster(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> fauna_conversations::backend::RoomRosterRead {
        read_roster(&self.client, channel_hex, home_nest_url).await
    }

    async fn read_policy_version(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
        version: u64,
    ) -> fauna_conversations::backend::RoomPolicyVersionRead {
        read_policy_version(&self.client, channel_hex, home_nest_url, version).await
    }
}

// The community class's generation read, on the same object for the same
// reason (`conversation-rooms.md` § The three classes → *Community*).
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::RoomGenerationReader for NestConversationsRpc {
    async fn read_generations(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> Option<Vec<fauna_conversations::backend::RoomGenerationWrap>> {
        read_generations(&self.client, channel_hex, home_nest_url).await
    }
}

// The community class's birth and keying doors, on the same object for the
// same reason (`conversation-rooms.md` § Implementation status today).
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::RoomCeremonyRpc for NestConversationsRpc {
    async fn room_create(
        &self,
        salt_hex: String,
        policy: Vec<u8>,
        reception_pubkey: Vec<u8>,
    ) -> Result<String, ConvRpcError> {
        create_room(&self.client, salt_hex, policy, reception_pubkey).await
    }

    async fn room_publish_generation(
        &self,
        room_id_hex: String,
        mint: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        publish_room_generation(&self.client, room_id_hex, mint).await
    }
    async fn room_invite(
        &self,
        invite: Vec<u8>,
        invitee_node: String,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        invite_to_room(&self.client, invite, invitee_node, home_nest_url).await
    }

    async fn room_accept_invite(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        accept_room_invite(&self.client, room_id_hex, reception_pubkey, home_nest_url).await
    }

    async fn room_backfill_generations(
        &self,
        room_id_hex: String,
        target_actor_id_hex: String,
        wraps: Vec<Vec<u8>>,
    ) -> Result<(), ConvRpcError> {
        backfill_room_generations(&self.client, room_id_hex, target_actor_id_hex, wraps).await
    }

    async fn room_pending_invitations(
        &self,
    ) -> Result<Vec<fauna_conversations::backend::PendingRoomInvitation>, ConvRpcError> {
        pending_room_invitations(Arc::clone(&self.nest)).await
    }

    async fn room_settle_invitation(&self, id: i64) -> Result<(), ConvRpcError> {
        settle_room_invitation(Arc::clone(&self.nest), id).await
    }

    async fn room_list_invites(
        &self,
        room_id_hex: String,
    ) -> Result<Vec<fauna_conversations::backend::PendingRoomInvite>, ConvRpcError> {
        list_room_invites(&self.client, room_id_hex).await
    }

    async fn room_revoke_invite(
        &self,
        room_id_hex: String,
        invitee_hex: String,
    ) -> Result<bool, ConvRpcError> {
        revoke_room_invite(&self.client, room_id_hex, invitee_hex).await
    }

    async fn room_remove(
        &self,
        room_id_hex: String,
        principal_hex: String,
    ) -> Result<u32, ConvRpcError> {
        remove_from_room(&self.client, room_id_hex, principal_hex).await
    }

    async fn room_leave(
        &self,
        room_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<u32, ConvRpcError> {
        leave_room(&self.client, room_id_hex, home_nest_url).await
    }

    async fn room_set_policy(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        set_room_policy(&self.client, room_id_hex, policy).await
    }

    async fn room_set_labelers(
        &self,
        room_id_hex: String,
        labelers: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        set_room_labelers(&self.client, room_id_hex, labelers).await
    }

    async fn room_transfer_ownership(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        transfer_room_ownership(&self.client, room_id_hex, policy).await
    }

    async fn room_set_reception_key(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
    ) -> Result<fauna_conversations::backend::RoomReceptionKeyBound, ConvRpcError> {
        set_room_reception_key(&self.client, room_id_hex, reception_pubkey).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::ConversationsRpc for NestConversationsRpc {
    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        self.client
            .channel_send(
                channel_id_hex,
                envelope,
                expect_no_commit_since,
                attachment_refs,
            )
            .await
            .map(|r| r.seq)
            .map_err(channel_send_error)
    }

    async fn channel_send_remote(
        &self,
        channel_id_hex: String,
        home_nest_url: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        self.client
            .channel_send_remote(
                channel_id_hex,
                home_nest_url,
                envelope,
                expect_no_commit_since,
                attachment_refs,
            )
            .await
            .map(|r| r.seq)
            .map_err(channel_send_error)
    }

    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        limit: i64,
        home_nest_url: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        self.client
            .channel_fetch(channel_id_hex, after, limit, home_nest_url)
            .await
            .map(fetched_records)
            .map_err(conv_rpc_error)
    }

    async fn keypackage_count(&self, actor_id_hex: String) -> Result<u64, ConvRpcError> {
        self.client
            .keypackage_count(actor_id_hex)
            .await
            .map(|r| r.count)
            .map_err(conv_rpc_error)
    }

    /// Degrades **every** failure to `Ok(None)` ("roster unreadable"), which is
    /// the seam's documented fail-safe: a federation hop that cannot answer
    /// for a foreign member, or a transient error all mean the same thing to the caller — do not guess at
    /// membership. The add path's fallback is a clear error to the user, so
    /// there is nothing an `Err` here would let it do better.
    ///
    /// The `Some`-ness pick lives HERE, once (the send-pick precedent,
    /// `direct-messages.md` § step 3b): a foreign-homed channel rides the
    /// distinct relay kind `channel.actors_remote`; same-nest stays on the
    /// plain read.
    async fn channel_actors(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<Option<Vec<String>>, ConvRpcError> {
        Ok(match home_nest_url {
            Some(url) => self
                .client
                .channel_actors_remote(channel_id_hex, url)
                .await
                .ok()
                .map(|r| r.actors),
            None => self
                .client
                .channel_actors(channel_id_hex)
                .await
                .ok()
                .map(|r| r.actors),
        })
    }

    async fn actor_by_handle(
        &self,
        handle: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        self.client
            .actor_by_handle(handle)
            .await
            .map(|opt| {
                opt.map(|r| fauna_conversations::backend::ResolvedHandle {
                    actor_id_hex: r.actor_id,
                    echoed_domain: r.domain,
                    addressable: r.addressable,
                })
            })
            .map_err(conv_rpc_error)
    }

    async fn actor_by_handle_remote(
        &self,
        domain: String,
        localpart: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        // The anon hop needs no home-nest connection — the shared free
        // function (`federation.md` § Peer-auth model).
        actor_by_handle_remote(&domain, &localpart).await
    }

    async fn keypackage_fetch(
        &self,
        actor_id_hex: String,
        peer_domain: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        self.client
            .keypackage_fetch(actor_id_hex, peer_nest_url(peer_domain))
            .await
            .map(|r| r.key_package)
            .map_err(conv_rpc_error)
    }

    async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<u64, ConvRpcError> {
        self.client
            .keypackage_upload(packages, last_resort)
            .await
            .map(|r| r.stored)
            .map_err(conv_rpc_error)
    }

    async fn welcome_deliver(
        &self,
        recipient_actor_id_hex: String,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        kind: WelcomeChannelKind,
        peer_domain: Option<String>,
    ) -> Result<(), ConvRpcError> {
        self.client
            .welcome_deliver(
                recipient_actor_id_hex,
                channel_id_hex,
                welcome_bytes,
                welcome_kind_to_wire(kind),
                peer_nest_url(peer_domain),
            )
            .await
            .map(|_| ())
            .map_err(conv_rpc_error)
    }

    async fn blob_put(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
        bytes: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        // Conversation attachments ride the canonical content-addressed
        // byte-source surface (`POST /api/v1/blob`, strict-multipart —
        // `bins/fauna-nest/src/blob_routes.rs::upload_blob` rejects any other
        // Content-Type with 400, so a raw-body POST here fails every time;
        // confirmed empirically 2026-07-20, no prior test exercised a real,
        // non-mock/non-injected FaunaMls attachment send on any client). The
        // bytes are already sealed client-side under the channel's
        // `derive_blob_key(epoch_secret)` (in `FaunaMlsBackend::send`), so the
        // nest stores them opaque and never holds an opening key; the sidecar
        // mime stays `application/octet-stream`
        // (`UploadSidecar::conversation_attachment`) — the real mime rides
        // inside the seal, same shape as `fauna_client::upload_gated_post_blob`.
        //
        // WHERE: the room's home nest (`conversation-rooms.md` § The home nest
        // → *Attachment bytes*). Same-nest (`None`): this member's own nest,
        // under its session bearer. Foreign-homed (`Some(home)`): DIRECT to the
        // home nest — bulk bytes never ride the federation channel — under a
        // short-lived write token this member's own nest relays from the home
        // nest (`fauna.conversations.blob.write_token.get`), accepted by the
        // same door's `BulkWriteAuth` arm. Either way the bytes land beside the
        // record whose plaintext `attachment_refs` pin them past the blob GC.
        //
        // NOTE: the nest's plaintext storage mode (and its nest-side
        // `process_media` on `/api/v1/blob` uploads) no longer exists — the nest
        // stores uploaded bytes opaquely, and `process_media` runs only
        // uploader-side. The shared-Rust rail + its round-trip test target that
        // opaque store.
        use fauna_nest_http::NestContentApi as _;
        let sidecar = fauna_media::sidecar::UploadSidecar::conversation_attachment().to_dag_cbor();
        // Normalized once: the same value keys the bearer cache and its verdict
        // slot, so the lookup below cannot miss on a whitespace difference.
        let home_key = home_nest_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        let result = match home_key.as_deref() {
            None => {
                self.nest
                    .auth()
                    .content_api()
                    .post_multipart_blob(fauna_nest_http::paths::blob::UPLOAD, sidecar, bytes)
                    .await
            }
            Some(home) => {
                let bearer: Arc<dyn fauna_nest_http::BearerSource> =
                    self.foreign_write_bearer(home, &channel_id_hex);
                fauna_nest_http::ReqwestNestContentApi::new(home, self.foreign_http.clone(), bearer)
                    .post_multipart_blob(fauna_nest_http::paths::blob::UPLOAD, sidecar, bytes)
                    .await
            }
        };
        result.map_err(|e| {
            // Prefer the MINT's classified verdict when the failure was the mint:
            // it is the only one that knows whether the home nest refused this
            // member (permanent) or could not be reached (retryable), and it
            // carries the localized sentence. `take` — a verdict is consumed by
            // the failure it explains.
            let minted_verdict = home_key.as_ref().and_then(|home| {
                self.foreign_mint_verdicts
                    .lock()
                    .unwrap()
                    .remove(&(home.clone(), channel_id_hex.clone()))
            });
            // The sealed cid is a diagnostic and belongs in the log, never in the
            // sentence the user reads (`conversations.md` § Errors & edge cases).
            tracing::warn!(
                sealed_cid = %sealed_cid_hex,
                error = %e,
                classified = ?minted_verdict,
                "conversations attachment upload failed"
            );
            // No mint verdict ⇒ the upload itself failed. Its text is the
            // RESPONDER's own words, so who answered decides whether the user may
            // read them — a foreign home nest never (`classify_blob_upload_error`);
            // the words are in the log line above either way.
            minted_verdict.unwrap_or_else(|| classify_blob_upload_error(home_key.is_some(), &e))
        })?;
        Ok(())
    }

    async fn blob_get(
        &self,
        _channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        // Public content-addressed download (`GET /api/v1/blob/{hash}` — no
        // bearer; `fauna-nest-http` `paths::blob::by_hash`) from the nest the
        // bytes rest on: this member's own nest for a same-nest channel, the
        // room's home nest — reached direct, plain WebPKI, integrity by content
        // address — for a foreign-homed one. A 404 is "blob not present"
        // (`Ok(None)` → the receive path skips that attachment).
        let auth = self.nest.auth();
        let (http, base) = match home_nest_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            None => (auth.http(), auth.nest_url()),
            Some(home) => (&self.foreign_http, home.trim_end_matches('/').to_string()),
        };
        let mut resp = http
            .get(format!("{base}/api/v1/blob/{sealed_cid_hex}"))
            .send()
            .await
            .map_err(|e| ConvRpcError::transient(format!("blob_get GET: {e}")))?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(ConvRpcError::transient(format!(
                "blob_get: HTTP {}",
                resp.status()
            )));
        }
        // Bounded on the read itself, so a home nest answering a named cid
        // with more bytes than its own door accepts cannot make this process
        // buffer them: a declared length over the limit is refused unread,
        // and a body that declares none is cut off where it passes the limit.
        if let Some(declared) = resp.content_length() {
            refuse_blob_body_over_limit(declared)?;
        }
        let mut body = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ConvRpcError::transient(format!("blob_get read body: {e}")))?
        {
            refuse_blob_body_over_limit((body.len() + chunk.len()) as u64)?;
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    }
}

/// Refuse an attachment blob body longer than the nest's inline blob door
/// accepts ([`fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT`]) — a
/// declared `Content-Length`, a running read total, or a finished read alike.
/// A rejection, never a transient fault: the same nest serves the same bytes
/// on every retry. Shared by the native and wasm `blob_get`
/// (`conversation-rooms.md` § The home nest → *The reader bounds what it
/// fetches*).
fn refuse_blob_body_over_limit(len: u64) -> Result<(), ConvRpcError> {
    let limit = fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT as u64;
    if len > limit {
        return Err(ConvRpcError::Rejected {
            message: format!(
                "blob_get: a body of {len} bytes is over the {limit}-byte inline blob limit"
            ),
        });
    }
    Ok(())
}

// ── Native MLS replica transport (`NestMlsReplicaTransport`) ───────────────────

/// Native [`MlsReplicaTransport`] seam impl — the `fauna.mls.{get,put}` sealed
/// replica plane (cross-device MLS group-state sync, `docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync) over `Arc<NestClient>`.
///
/// The seam keeps `MlsStateSync` / `FaunaCommitGate` non-generic (the dyn-erased
/// transport, design § "dyn-erasure"); this adapter is the one native place the
/// seam meets the wire, delegating to `fauna_client_mls_sync`'s
/// `rpc_transport_{get,put}` helpers (which own the encode + the
/// `fauna.mls.conflict` → [`PutOutcome::Conflict`] classification). Shared by
/// every native app, exactly as [`NestConversationsRpc`] above is — wired
/// once per client via `MlsStateSync::new(Box::new(NestMlsReplicaTransport::new(nest)), …)`.
/// The wasm twin is [`WsMlsReplicaTransport`] over `WsRpcClient` (below).
#[cfg(not(target_arch = "wasm32"))]
pub struct NestMlsReplicaTransport {
    nest: Arc<NestClient>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestMlsReplicaTransport {
    pub fn new(nest: Arc<NestClient>) -> Self {
        Self { nest }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl MlsReplicaTransport for NestMlsReplicaTransport {
    async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_get(&self.nest, path).await
    }

    async fn get_hash(&self, path: String) -> Result<Option<[u8; 32]>, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_get_hash(&self.nest, path).await
    }

    async fn put(
        &self,
        path: String,
        blob: Vec<u8>,
        base: ReplicaBase,
    ) -> Result<PutOutcome, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_put(&self.nest, path, blob, base).await
    }
}

// ── Wasm MLS replica transport (`WsMlsReplicaTransport`) ───────────────────────

/// Wasm [`MlsReplicaTransport`] seam impl — the browser twin of
/// [`NestMlsReplicaTransport`], carrying the `fauna.mls.{get,put}` sealed replica
/// plane (cross-device MLS group-state sync, `docs/goal/behavior/devices.md`
/// § Cross-device MLS group-state sync) over the `Rc`-based `WsRpcClient`.
///
/// Holds a `WsRpcClient` **by value** (a cheap `Rc` handle), exactly as the
/// sibling [`WsConversationsRpc`] does — not an `Arc`, since the wasm transport is
/// already `Rc`-cloneable and single-threaded. The two `get`/`put` methods are the
/// same one-line delegations to the shared `rpc_transport_{get,put}` helpers the
/// native adapter uses (they own the wire encode + the `fauna.mls.conflict` →
/// [`PutOutcome::Conflict`] classification); the generic helpers instantiate at the
/// concrete `WsRpcClient` here, so their futures' non-`Send`ness satisfies the
/// `#[async_trait(?Send)]` wasm arm of the seam. Wired once in the web leg via
/// `MlsStateSync::new(Box::new(WsMlsReplicaTransport::new(client)), &keypair)`
/// (`fauna-wasm` `WasmConversationsManager::with_conversations`).
///
/// Generic over the requester since the succession leg (2026-08-20), defaulting
/// to `WsRpcClient` so every existing call site reads unchanged. The two helpers
/// it delegates to were **always** generic (`rpc_transport_{get,put}<R:
/// RpcRequester>`); only this adapter pinned the concrete type, which left the
/// ceremony's short-lived successor session — a fixed-bearer
/// [`fauna_rpc_wasm::TokenWsRpcClient`], since no JS token provider exists for an
/// identity the page has not signed in as yet — unable to reach the replica plane
/// at all. Widening here beat a second adapter: one transport, one wire encode,
/// one conflict classification.
#[cfg(target_arch = "wasm32")]
pub struct WsMlsReplicaTransport<R = fauna_rpc_wasm::WsRpcClient> {
    client: R,
}

#[cfg(target_arch = "wasm32")]
impl<R> WsMlsReplicaTransport<R> {
    pub fn new(client: R) -> Self {
        Self { client }
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl<R> MlsReplicaTransport for WsMlsReplicaTransport<R>
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass + core::fmt::Display,
{
    async fn get(&self, path: String) -> Result<Option<Vec<u8>>, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_get(&self.client, path).await
    }

    async fn get_hash(&self, path: String) -> Result<Option<[u8; 32]>, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_get_hash(&self.client, path).await
    }

    async fn put(
        &self,
        path: String,
        blob: Vec<u8>,
        base: ReplicaBase,
    ) -> Result<PutOutcome, MlsTransportError> {
        fauna_client_mls_sync::rpc_transport_put(&self.client, path, blob, base).await
    }
}

// ── Native inbound-push seam (`NestConversationsPush`) ─────────────────────────

/// Map a decoded `PushEvent` to the receive loop's protocol-agnostic
/// `ConvPushEvent` (`fauna_conversations::backend`). `None` for an unrelated kind
/// — the kind-filtered subscribers should never yield those, but the mapping
/// stays total + pure so it is unit-testable without a live `NestClient`.
#[cfg(not(target_arch = "wasm32"))]
fn push_event_to_conv(
    ev: fauna_client::PushEvent,
) -> Option<fauna_conversations::backend::ConvPushEvent> {
    use fauna_client::PushEvent;
    use fauna_conversations::backend::{ConvPushEvent, WelcomeNudge};
    match ev {
        PushEvent::Welcome(w) => Some(ConvPushEvent::Welcome(WelcomeNudge {
            channel_id_hex: w.channel_id,
            welcome_bytes: w.welcome_bytes,
            // Decode the wire `channel_type` so the receive loop routes a
            // scheduling welcome to the calendar-apply drain, not a chat thread.
            kind: wire_channel_type_to_kind(w.channel_type, w.group_id),
            // The channel's home nest (cross-nest Welcome) — recorded so a later
            // `channel.fetch` of this channel relays there; `None` same-nest.
            home_nest_url: w.nest_url,
            // The nest-stamped sharer id for a folder Welcome — the recipient
            // contact gate reads it (auto/knock/suppress); `None` for other kinds.
            shared_by: w.shared_by,
            // The home-nest-resolved set name for a folder Welcome — threaded
            // into the accept-time foreign-set record on a cross-nest share.
            set_name: w.set_name,
            // The home-nest-resolved access grant, threaded the same way — it
            // decides whether this client offers a folder binding. Advisory
            // only; never an authz input.
            access: w.access,
            // The home nest's deployment identity (byte-plane pin root) + the
            // owner-chosen cadence, threaded into the accept-time foreign record.
            home_nest_actor_id: w.home_nest_actor_id,
            // The cross-nest owner label (its own nest verified the domain).
            shared_by_handle: w.shared_by_handle,
            shared_by_domain: w.shared_by_domain,
            // The sealed set name — what names a cross-nest set once joined.
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                w.set_name_sealed.as_deref().map(|b| &b[..]),
                w.set_name_hash.as_deref().map(|b| &b[..]),
            ),
        })),
        PushEvent::ChannelMessage(_) => Some(ConvPushEvent::ChannelMessage),
        _ => None,
    }
}

/// Decode the wire `WelcomePayload.channel_type` (+ `group_id`) into the
/// protocol-agnostic [`WelcomeChannelKind`] the receive loop routes on — the
/// receive-side inverse of [`welcome_kind_to_wire`]. The tag strings match the
/// `WelcomeKind` snake_case wire tags (`fauna_protocol::conversations::WelcomeKind`):
/// `"scheduling"` → the mailbox-less CalDAV iMIP rail, `"group"` → an n-way chat
/// (carrying the hex group id), and `"dm"` or any unrecognized tag (a kind a
/// newer peer added) → a 1:1 chat DM. Every current sender stamps a tag
/// (the nest derives it from the closed [`WelcomeKind`]), so an absent one
/// takes the same unrecognized-tag arm rather than a path of its own.
///
/// `pub` + un-cfg'd (native **and** wasm): the native drain/push arms and the web
/// (wasm) welcome drain (`WebInboxApply::apply_welcome`, `fauna-wasm`) both route
/// on the full tri-state, from this one source of the `channel_type` vocabulary,
/// so every plane agrees on what `"folder"` decodes to (priority #2/#4).
pub fn wire_channel_type_to_kind(
    channel_type: Option<String>,
    group_id: Option<String>,
) -> WelcomeChannelKind {
    match channel_type.as_deref() {
        Some("scheduling") => WelcomeChannelKind::Scheduling,
        Some("group") => WelcomeChannelKind::Group {
            group_id_hex: group_id.unwrap_or_default(),
        },
        // A cross-user shared folder Welcome (folders.md § Sharing) — routed
        // to the folder pending-share surface, not a chat thread. Carries the
        // group id like a group. Must precede the `_ => Dm` default, else a
        // folder Welcome would surface a phantom DM.
        Some("folder") => WelcomeChannelKind::Folder {
            group_id_hex: group_id.unwrap_or_default(),
        },
        _ => WelcomeChannelKind::Dm,
    }
}

/// The two kind-filtered subscribers the loop selects over, behind one async
/// mutex. The `ConversationsSession` receive loop is the **sole** consumer, so
/// the guard held across the inner `select!` recv never contends; broadcast
/// `recv` is cancel-safe, so dropping the future when the loop's outer `select!`
/// picks its ticker loses nothing.
#[cfg(not(target_arch = "wasm32"))]
struct ConvPushSubs {
    welcome: fauna_client::push::KindSubscriber,
    channel: fauna_client::push::KindSubscriber,
    /// `fauna.mail.received` — the inbound-mail arrival wake. The receive loop
    /// drives both the conv and mail rails, so this rides the same push seam; on
    /// it the loop re-polls the mail read-feeds promptly (`smtp-server.md` §
    /// Inbound client receive → Arrival push). The payload is irrelevant (a bare
    /// wake), so any frame maps to `ConvPushEvent::MailReceived`.
    mail: fauna_client::push::KindSubscriber,
    /// `fauna.mail.flags_changed` — an `INBOX` flag changed somewhere, the mail
    /// rail's read-state wake (`mail-app-surface.md` § Read state). Held here
    /// beside the arrival wake, the rail's own subscription, so every app on
    /// this loop hears it whatever its central push dispatch does with the
    /// kind. A bare wake: any frame maps to `ConvPushEvent::MailFlagsChanged`.
    mail_flags: fauna_client::push::KindSubscriber,
    /// `fauna.bridges.push.conversation_changed` — a bridged room changed (a
    /// deposit, a room report, a receipt). The bridged rail rides this loop
    /// like mail does; the payload names a room the loop does not need (it
    /// re-reads the rooms and the inbox from its cursor), so any frame maps
    /// to `ConvPushEvent::BridgedChanged`.
    bridged: fauna_client::push::KindSubscriber,
    /// `fauna.addressbook.changed` — a card or book write landed in one of this
    /// actor's address books, on any device or through the MDA. Rides this seam
    /// for the same reason the mail wake does: the receive loop is the one place
    /// that already means "something happened, act now", and the contacts
    /// reconcile walk lives behind the index launcher the loop holds. Payload
    /// irrelevant (a bare wake, and deliberately so — the walk re-reads by ctag),
    /// so any frame maps to `ConvPushEvent::AddressBookChanged`.
    addressbook: fauna_client::push::KindSubscriber,
    /// `fauna.sync.changed` — a sync record landed in one of this actor's file
    /// sets, fanned out to every connected same-nest participant. Rides this seam
    /// for the same reason the address-book wake does: the File reconcile walk
    /// lives behind the index launcher the receive loop already holds, and the
    /// loop is the one place that already means "something happened, act now".
    ///
    /// Payload deliberately dropped (a bare wake) — the walk is a cross-set drain
    /// with no per-set cursor, so the nudge's `folder` name would narrow
    /// nothing, and it is a sealed label this seat may not be able to render.
    sync_files: fauna_client::push::KindSubscriber,
    /// The shared reconnect signal (`NestClient::subscribe_reconnects`, a
    /// `watch<u64>` bumped on every `Connected` **after the first**). Not a push
    /// *kind* — it rides this seam because the receive loop's push arm is the one
    /// place that already means "something happened, sweep now", and a reconnect
    /// is exactly that: the three subscriptions above survive a reconnect (the
    /// broker outlives the per-connection dispatcher), but any frame the nest
    /// broadcast *while we were down* was never delivered and only a pull
    /// recovers it (`transport.md` § Reconnect & resync).
    reconnects: tokio::sync::watch::Receiver<u64>,
}

/// Native `ConversationsPush` seam impl — the inbound-push twin of
/// [`NestConversationsRpc`]. Subscribes the shared `NestClient`'s long-lived push
/// broker (which survives reconnects, `fauna-client::push`) to
/// `fauna.conversations.welcome.received` + `.channel.message` **+
/// `fauna.mail.received`** (the unified receive loop drives both the conv and mail
/// rails), decoding each `PushEvent` into the loop's `ConvPushEvent`. Built once per
/// native app by
/// the FFI factory (`fauna-ffi::conversations_session`) and injected into
/// `ConversationsSession::from_parts`, exactly as `NestConversationsRpc` is. The
/// browser drives its own `future_to_promise` poll loop, so there is no wasm twin.
#[cfg(not(target_arch = "wasm32"))]
pub struct NestConversationsPush {
    subs: tokio::sync::Mutex<ConvPushSubs>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestConversationsPush {
    pub fn new(nest: Arc<NestClient>) -> Self {
        Self {
            subs: tokio::sync::Mutex::new(ConvPushSubs {
                welcome: nest.subscribe_kind("fauna.conversations.welcome.received"),
                channel: nest.subscribe_kind("fauna.conversations.channel.message"),
                mail: nest.subscribe_kind("fauna.mail.received"),
                mail_flags: nest.subscribe_kind("fauna.mail.flags_changed"),
                bridged: nest.subscribe_kind(
                    fauna_client_bridges::conversations::CONVERSATION_CHANGED_PUSH_KIND,
                ),
                addressbook: nest.subscribe_kind("fauna.addressbook.changed"),
                sync_files: nest.subscribe_kind("fauna.sync.changed"),
                reconnects: nest.subscribe_reconnects(),
            }),
        }
    }
}

/// Build the inbound conversations push source [`from_parts`]/[`from_manager`]
/// subscribes — unless a **test-capable build** has been told to suppress it via the
/// `FAUNA_E2E_SUPPRESS_CONV_PUSH` env var, in which case it returns `None` so the
/// session runs the ticker + durable inbox-apply **drain** alone. This is the
/// native knob that forces the missed-push poll path for the layer-5 receive proof
/// (`docs/goal/architecture/api-layers.md` § Inbox & Messaging, layer 5): a real
/// engine-bearing GUI receiver with its push arm off must still receive + decrypt a
/// Welcome from the durable queue. **Not user-facing.** Every native session-build
/// site (`fauna-ffi` factory + linux/tui `conv_backend`) routes through here so the
/// suppression is uniform (priority #1).
///
/// ⚠ **Compile-gated outer, env inner** (`e2e-automation-surface-gating.md` §
/// convention 15), the `always_resident::{debounce_delay, rescan_interval}` shape:
/// the read below exists only in a build carrying `debug_assertions` or the crate's
/// opt-in `e2e-agent` feature, and the production twin returns the live push source
/// unconditionally. Until 2026-08-21 the only gate here was
/// `cfg(not(target_arch = "wasm32"))` — a *platform* gate, not a build-flavor one —
/// so the read compiled into every native **release** artifact and anyone who could
/// influence a shipped app's launch environment could kill real-time message
/// delivery on six apps with an *empty* value (`var_os(…).is_some()`), announced by
/// one `tracing::warn!` no user sees. `debug_assertions` covers the debug e2e
/// flavors; linux/tui forward the feature from their own `e2e-agent`, and
/// `fauna-ffi` from its `test-helpers`, so a release-profile e2e build keeps the
/// knob (never on a dep line — convention 15 rule (b)).
///
/// **Native-only because the *session loop* is native-only, not because web lacks a
/// push arm — web has one as of 2026-07-12.** The SPA cannot run
/// `start_receive_loop` (that `tokio::select!` is `cfg(not(wasm32))`; the `Rc`-based
/// wasm RPC client is `!Send`), so it hand-mirrors the loop's arms in
/// `apps/fauna-web/src/lib/conversations.ts` — a 30s backstop ticker plus a push arm
/// off the same `fauna.conversations.{channel.message,welcome.received}` +
/// `fauna.mail.received` kinds, funnelled through one serialized pump. Web's
/// equivalent lever is therefore the mirror of this one: a **poll**-suppression knob
/// (`setConvPollSecs`, reached from the e2e automation surface as
/// `window.__fauna_setConvPollSecs`), built 2026-08-15 and consumed by
/// `tests/e2e-unified/tests/test_conv_rail_push_wakes_web.py` — without it the SPA's
/// 2 s e2e-agent cadence masks a dead push arm. `transport.md` § Push events owns the
/// pairing.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(debug_assertions, feature = "e2e-agent")
))]
pub fn conv_push_source(
    nest: Arc<NestClient>,
) -> Option<Arc<dyn fauna_conversations::backend::ConversationsPush>> {
    if std::env::var_os("FAUNA_E2E_SUPPRESS_CONV_PUSH").is_some() {
        tracing::warn!(
            "FAUNA_E2E_SUPPRESS_CONV_PUSH set — conversations push arm disabled \
             (drain-only receive; e2e missed-push backstop proof)"
        );
        return None;
    }
    Some(Arc::new(NestConversationsPush::new(nest)))
}

/// Production twin of [`conv_push_source`] — no env read exists in this build, so the
/// push arm is live by construction rather than by a promise nobody could enforce.
/// Same signature, so the three native session-build call sites compile unchanged.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(debug_assertions, feature = "e2e-agent"))
))]
pub fn conv_push_source(
    nest: Arc<NestClient>,
) -> Option<Arc<dyn fauna_conversations::backend::ConversationsPush>> {
    Some(Arc::new(NestConversationsPush::new(nest)))
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_conversations::backend::ConversationsPush for NestConversationsPush {
    async fn next_event(&self) -> Option<fauna_conversations::backend::ConvPushEvent> {
        use fauna_conversations::backend::ConvPushEvent;
        use tokio::sync::broadcast::error::RecvError;
        let mut guard = self.subs.lock().await;
        let subs = &mut *guard;
        loop {
            tokio::select! {
                w = subs.welcome.recv() => match w {
                    Ok(ev) => match push_event_to_conv(ev) {
                        Some(c) => return Some(c),
                        None => continue, // kind-filtered: unreachable, stays total
                    },
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return None,
                },
                c = subs.channel.recv() => match c {
                    // An arrival or a lag both resolve to "poll now"; the loop's
                    // per-channel cursor catches up regardless of coalescing.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::ChannelMessage),
                    Err(RecvError::Closed) => return None,
                },
                m = subs.mail.recv() => match m {
                    // Mail arrived — wake the loop's mail poll (the per-mailbox
                    // `after_uid` cursor dedups, so coalescing arrivals is fine). The
                    // payload is a bare wake, so any frame maps to `MailReceived`.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::MailReceived),
                    Err(RecvError::Closed) => return None,
                },
                g = subs.mail_flags.recv() => match g {
                    // A flag changed — drain the changes from the cursor. A lag
                    // is the same wake: the cursor carries correctness.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::MailFlagsChanged),
                    Err(RecvError::Closed) => return None,
                },
                b = subs.bridged.recv() => match b {
                    // A bridged room changed — a lag is the same wake: the
                    // row cursor carries correctness.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::BridgedChanged),
                    Err(RecvError::Closed) => return None,
                },
                a = subs.addressbook.recv() => match a {
                    // A book moved. A lag resolves the same way an arrival does:
                    // the walk reconciles the whole corpus by ctag, so coalescing
                    // any number of card writes into one wake loses nothing —
                    // which is exactly why this event carries no payload.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::AddressBookChanged),
                    Err(RecvError::Closed) => return None,
                },
                f = subs.sync_files.recv() => match f {
                    // A set moved. A lag resolves the same way an arrival does:
                    // the walk drains the whole cross-set listing, so coalescing
                    // any number of file writes — across any number of sets —
                    // into one wake loses nothing. Which is also why the payload
                    // goes unread.
                    Ok(_) | Err(RecvError::Lagged(_)) => return Some(ConvPushEvent::SyncFilesChanged),
                    Err(RecvError::Closed) => return None,
                },
                r = subs.reconnects.changed() => match r {
                    // The WS came back (bumps only on `Connected` *after* the first,
                    // so this never fires for the initial connect). `changed()` marks
                    // the value seen, so each bump wakes the loop exactly once and
                    // coalesced bumps cost one redundant, cursor-dedup'd sweep.
                    Ok(()) => return Some(ConvPushEvent::Reconnected),
                    // Sender gone — the `NestClient` dropped, which also closes the
                    // three subscriptions above. Report the source as closed rather
                    // than `continue`: a dropped sender makes `changed()` return
                    // `Err` immediately and forever, so looping here would spin.
                    Err(_) => return None,
                },
            }
        }
    }
}

/// The per-actor mail key material the source derives once from the account's
/// mail custody (`fauna.state.mail`) and reuses for every page — six 32-byte secrets
/// plus a length, not the "3×32 B" an earlier revision of this comment claimed.
///
/// **Deliberately NOT `Copy`, and zeroize-on-drop** (`key-material-hierarchy.md`
/// § Plaintext key lifetime on bridges → *Carrier shape*). This is the one
/// shared lazily-derived cache every mail-keyed consumer reads, and it holds
/// `msek` itself — the Path B root from which the index-segment key, the
/// recipient-mail secret and every mail-epoch root all derive. A `Copy` carrier
/// scatters implicit duplicates the type system says nothing about, and on a
/// *cache* the count is unbounded; `Copy` and `Drop` are mutually exclusive in
/// Rust, so being non-`Copy` is what makes zeroize-on-drop expressible at all.
///
/// The `Copy` this used to derive was justified as "so the cache lock is never
/// held across an `.await`" — which `Clone` delivers identically, since the
/// clone happens inside the guard's own statement and the guard drops before
/// the await ([`MailKeyCache::get`]). Nothing was traded away to drop it.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct MailKeys {
    /// The account's complete **standing** recipient-mail key set — the
    /// current MSEK's keypair first, then one per prior grace generation
    /// (`MailConfig.prior_mseks`, cap-2 — [`SNAPSHOT_GRACE_KEYPAIRS`] total),
    /// from the ONE shared derivation the MDA's snapshot is built from
    /// (`derive_standing_mail_keypairs`; `owner-key-material.md` § Path
    /// B-sibling-2). Never empty once mail is enabled. `[0]` is also the key
    /// the per-user spam model is sealed to.
    standing: Vec<StandingMailKeypair>,
    /// Raw `mail.msek` — the calendar seal/unseal key `apply_inbound_reply_from_mail`
    /// re-seals the merged event with.
    msek: [u8; 32],
    /// This actor's id — the organizer whose stored event an inbound iTIP `REPLY`
    /// updates.
    actor_id: [u8; 32],
    /// The client's mail-epoch roots for the content-sealing-epochs opener chain
    /// (design § 4/§ 5): `[0]` = the current MSEK's root
    /// ([`derive_mail_epoch_root`]), then one per prior grace generation, aligned
    /// with `standing`. The receive path passes them (as `&[&[u8; 32]]`) to the
    /// epoch opener. Only the current root is the common case (no rotation yet).
    epoch_roots: Vec<[u8; 32]>,
    /// The prior grace MSEK generations (`MailConfig.prior_mseks`, newest first,
    /// capped beside `standing`) — held so [`Self::index_ring`] can open a
    /// mail/calendar index blob sealed before a rotation. Empty until the
    /// account first rotates.
    prior_mseks: Vec<[u8; 32]>,
}

#[cfg(not(target_arch = "wasm32"))]
impl MailKeys {
    /// The key set from the account's mail custody: the live MSEK, then its
    /// prior grace generations (already capped by the caller), every derived
    /// key aligned with that one history.
    fn from_custody(actor_id: [u8; 32], msek: &[u8; 32], prior_mseks: &[[u8; 32]]) -> Self {
        let mseks: Vec<[u8; 32]> = std::iter::once(*msek)
            .chain(prior_mseks.iter().copied())
            .collect();
        // Deref out of the `Zeroizing` derivation wrapper: the wrapper drops
        // (zeroizing) at the end of each statement, and the destination is
        // `MailKeys`, which is itself zeroize-on-drop.
        let epoch_roots = mseks.iter().map(|m| *derive_mail_epoch_root(m)).collect();
        Self {
            standing: derive_standing_mail_keypairs(&mseks),
            msek: *msek,
            actor_id,
            epoch_roots,
            prior_mseks: prior_mseks.to_vec(),
        }
    }

    /// The mail/calendar index key ring: the current generation plus one grace
    /// key per prior MSEK. Both client readers of the `__index` mail/calendar
    /// slice — the builder's resume and the local search arm — take it from
    /// here, so a rotation does not cut the user off from the index sealed
    /// before it (`owner-key-material.md` § Path B-sibling-4).
    fn index_ring(&self) -> fauna_client_index::MailcalKeyRing {
        fauna_client_index::MailcalKeyRing::from_msek_and_priors(&self.msek, &self.prior_mseks)
    }

    /// Open one `inbox.fetch` / `sent.fetch` record (both decrypt layers,
    /// either suite) under this key set: the epoch chain over the roots, then
    /// the standing arm over the complete standing set — the one shared
    /// opener (`fauna_mail::open_inbound_record_with_keys`) web uses too.
    /// `seal_instant` is the record's `stored_at` (`0` when unknown), never
    /// `internal_date` nor a sender-supplied header.
    fn open_record(
        &self,
        sealed_envelope: &[u8],
        seal_instant: u64,
    ) -> Result<Vec<u8>, fauna_mail::InboundOpenError> {
        let epoch_roots: Vec<&[u8; 32]> = self.epoch_roots.iter().collect();
        fauna_mail::open_inbound_record_with_keys(
            sealed_envelope,
            &epoch_roots,
            seal_instant,
            &self.standing,
        )
    }
}

/// The one lazily-derived [`MailKeys`] cache every mail-keyed consumer shares.
///
/// Three consumers used to spell this out independently — the INBOX/Sent read
/// feeds, the scheduling sink, and (as of the index builder's lifecycle) the
/// content-index launcher. Each spelling was a separate mail-material read at
/// login and a separate copy of the same "not ready yet, retry next tick" rule,
/// so they are one type now (priority #4 — resolve drift rather than add a
/// fourth copy of it). The MSEK comes from the account's mail custody
/// (`fauna.state.mail`, through [`fauna_client_config::MailStore`]).
///
/// **This is why the MSEK never leaves Rust**: consumers hold an `Arc` of the
/// cache and ask it for keys inside shared Rust; nothing hands the raw MSEK to
/// app glue (`conversations.md` § Architectural rules #2,
/// `key-material-hierarchy.md` § Path B-sibling-4).
#[cfg(not(target_arch = "wasm32"))]
pub struct MailKeyCache {
    /// The session's connection — whose identity names the actor the keys
    /// belong to.
    nest: Arc<NestClient>,
    /// The account's mail custody — the MSEK and its grace window.
    mail: Arc<dyn fauna_client_config::MailStore>,
    /// `None` until derived — and while mail stays unprovisioned, which is not
    /// an error state (see [`Self::get`]).
    keys: std::sync::Mutex<Option<MailKeys>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl MailKeyCache {
    pub fn new(nest: Arc<NestClient>, mail: Arc<dyn fauna_client_config::MailStore>) -> Arc<Self> {
        Arc::new(Self {
            nest,
            mail,
            keys: std::sync::Mutex::new(None),
        })
    }

    /// The per-actor mail keys, deriving + caching them on first use.
    ///
    /// `None` means "not ready" — no identity keypair yet, a transient
    /// custody read failure, or mail simply not enabled (no MSEK) — in
    /// which case the caller treats its pass as a no-op and retries next tick.
    /// Never holds the cache lock across the `.await`.
    async fn get(&self) -> Option<MailKeys> {
        // `.clone()` rather than a `Copy` read out of the guard: the clone
        // happens inside this statement, so the guard still drops before the
        // `.await` below — the property the old `Copy` was justified by, with
        // no implicit duplication (see [`MailKeys`]).
        if let Some(k) = self.keys.lock().unwrap().clone() {
            return Some(k);
        }
        let derived = load_mail_keys(&self.nest, self.mail.as_ref()).await?;
        *self.keys.lock().unwrap() = Some(derived.clone());
        Some(derived)
    }

    /// Drop the cached keys and re-derive from the current mail custody — the
    /// one place the cache is ever refreshed once derived. Called by the page
    /// opener when a record misses under the cached set, BEFORE it declares
    /// the record unopenable: the account's keys can change under a running
    /// session (a rotate-mail-keys, a mailbox turned off and back on — both
    /// from this or another device), and a miss against a *stale* set is not
    /// a fact about the record. One custody read per miss, never per tick.
    async fn refresh(&self) -> Option<MailKeys> {
        *self.keys.lock().unwrap() = None;
        self.get().await
    }
}

/// The canonical nest-backed inbound-mail read-feed source — the **receive** twin
/// of the conversations SMTP send sink, shared by every native app (linux
/// directly; macOS / iOS / Windows / Android via the `fauna-ffi`
/// `conversations_session` factory). Reads one server mailbox over `EmailClient`
/// and opens each sealed record to plaintext RFC 5322 with the recipient's
/// MSEK-derived HPKE secret via the one shared `fauna_mail::open_inbound_record`
/// two-layer decrypt; the transport-free `fauna_conversations::poll_inbound_mail`
/// driver does parse + bucket + ingest (priority #2 — no per-app crypto beyond
/// that single call).
///
/// As the native inbound-mail chokepoint it ALSO routes an inbound iTIP `REPLY`
/// (a `text/calendar; method=REPLY` part on the **INBOX** feed) to the calendar
/// layer: `CalDavClient::apply_inbound_reply_from_mail` merges the responder's
/// PARTSTAT into the organizer's stored event and re-PUTs it (caldav-server.md §
/// The one operation with a cost — the v1 client-driven reply-merge) — but only
/// for the attendee the delivery door authenticated as the sender, and never
/// from a copy this pass's scorer files Junk (§ Who may mutate an existing event
/// over the inbound rail → *The mail rail*); a refusal is recorded for the Events
/// page. It runs as a
/// detached best-effort task so a calendar write never stalls mail delivery. (Only
/// INBOX carries inbound REPLYs to merge — our own `Sent` copies are organizer
/// REQUESTs / plain mail, never a REPLY into our own event.)
///
/// The key material derives **lazily** on first fetch: the MSEK lives in the
/// account's mail custody, so until mail is enabled there is nothing to
/// derive and `fetch` is a graceful no-op (empty page; the loop idles on this rail
/// and retries next tick). The MSEK never leaves Rust — the client only ever
/// observes decrypted messages via the manager (conversations.md § Architectural
/// rules #2).
#[cfg(not(target_arch = "wasm32"))]
pub struct NestMailInboundSource {
    nest: Arc<NestClient>,
    email: EmailClient<Arc<NestClient>>,
    /// Typed user-tier client for the on-device spam scorer's two reads —
    /// `fetch_spam_model` (the sealed per-user model) + `get_spam_scoring_policy`
    /// (the admin-effective `spam_folder` threshold + Bayesian knobs). Used only by
    /// the `INBOX` source's [`Self::begin_pass`]; the `Sent` source never scores.
    mail_account: MailAccountClient<Arc<NestClient>>,
    /// Which of the caller's own server-side mailboxes this source reads.
    /// `Inbox` is the inbound feed (`fauna.email.inbox.fetch`); `Sent` is the
    /// actor's own outbound copies (`fauna.email.sent.fetch`) — surfaced so the
    /// unified conversations view shows both halves of a thread, incl. mail
    /// submitted from an external MUA (`smtp-server.md` § Inbound client
    /// receive). The decrypt is identical (both sealed to the same MSEK-derived
    /// recipient key); only the read kind and the INBOX-only work done on a
    /// delivery (the iTIP-REPLY route, the spam scorer) differ.
    mailbox: MailFeed,
    /// Lazily-derived key cache, **shared** with the `Sent` twin, the scheduling
    /// sink and the content-index launcher, so only the first consumer to need
    /// keys loads the mail custody this launch.
    keys: Arc<MailKeyCache>,
    /// This pass's on-device INBOX spam scorer — the native twin of the web
    /// `WasmConversationsManager::spam_scorer`. `Some` only on the `INBOX` source,
    /// only while a *trained* model + the scoring policy were fetched this
    /// pass ([`Self::begin_pass`]); [`Self::fetch`] feeds each just-decrypted
    /// message to it, [`Self::end_pass`] flushes the accumulated disposition.
    /// `None` = cold start (no model) / mail off → no scoring,
    /// so no un-based watermarking (the `InboxSpamScorer` construction contract).
    spam_scorer: std::sync::Mutex<Option<InboxSpamScorer>>,
    /// Where a refused mailed `REPLY` is recorded — the session manager's
    /// inbox ([`fauna_conversations::refused_changes`]), which reaches the
    /// account plane once the host's account store is up.
    refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestMailInboundSource {
    /// Build the `INBOX` + `Sent` read-feed pair over one connection, sharing a
    /// single lazily-derived key cache (so only the first poll across either reads
    /// the mail custody). Both are registered on the session via
    /// `ConversationsSession::register_mail_receive`.
    pub fn inbox_and_sent(
        nest: Arc<NestClient>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
    ) -> (Arc<Self>, Arc<Self>) {
        let keys = MailKeyCache::new(Arc::clone(&nest), mail);
        Self::inbox_and_sent_over(nest, keys, refused)
    }

    /// [`Self::inbox_and_sent`] over a caller-supplied cache — how a client that
    /// also runs the content-index launcher (or the scheduling sink) gets all of
    /// them onto **one** custody read per launch instead of one each.
    pub fn inbox_and_sent_over(
        nest: Arc<NestClient>,
        keys: Arc<MailKeyCache>,
        refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
    ) -> (Arc<Self>, Arc<Self>) {
        let inbox = Arc::new(Self {
            nest: Arc::clone(&nest),
            email: EmailClient::new(Arc::clone(&nest)),
            mail_account: MailAccountClient::new(Arc::clone(&nest)),
            mailbox: MailFeed::Inbox,
            keys: Arc::clone(&keys),
            spam_scorer: std::sync::Mutex::new(None),
            refused: Arc::clone(&refused),
        });
        let sent = Arc::new(Self {
            email: EmailClient::new(Arc::clone(&nest)),
            mail_account: MailAccountClient::new(Arc::clone(&nest)),
            nest,
            mailbox: MailFeed::Sent,
            keys,
            spam_scorer: std::sync::Mutex::new(None),
            refused,
        });
        (inbox, sent)
    }

    /// This source's mail keys — see [`MailKeyCache::get`] for the `None`
    /// (not-ready) contract the poll treats as a no-op.
    async fn keys(&self) -> Option<MailKeys> {
        self.keys.get().await
    }

    /// Detach a best-effort iTIP-REPLY merge for an INBOX record. The
    /// `text/calendar`/`REPLY` gate, the authenticated-sender check and the
    /// PARTSTAT merge all live in the shared
    /// `CalDavClient::apply_inbound_reply_from_mail` (single source of truth — no
    /// gate duplicated here), so ordinary mail is a cheap `NotCalendarReply` no-op.
    /// A `Refused` outcome is logged at `warn` and recorded as the user-facing
    /// notice (the session's refused-change inbox, the row composed by
    /// `InboundReplyOutcome::refused_mail_change_record` over the same bytes).
    /// Errors are logged, never surfaced — the mail itself already delivered, and an
    /// offline / mid-reconnect nest re-applies on the next poll of the same record
    /// (the merge is idempotent).
    ///
    /// The caller runs this only for a copy the scorer did NOT file Junk
    /// (caldav-server.md § Server-side auto-schedule, invitation rule 3).
    fn spawn_reply_merge(&self, rfc5322: &[u8], keys: &MailKeys) {
        let nest = Arc::clone(&self.nest);
        let actor_id = keys.actor_id;
        let msek = keys.msek;
        // The whole MSEK history, so a REPLY to an event created before a
        // mail-key rotation still finds it; zeroized when the task ends.
        let prior_mseks = zeroize::Zeroizing::new(keys.prior_mseks.clone());
        let raw = rfc5322.to_vec();
        let refused = Arc::clone(&self.refused);
        tokio::spawn(async move {
            match CalDavClient::new(Arc::clone(&nest))
                .apply_inbound_reply_from_mail(&actor_id, &msek, &prior_mseks, &raw, now_secs())
                .await
            {
                Ok(outcome) => {
                    if let Some(row) = outcome.refused_mail_change_record(&raw, now_secs()) {
                        tracing::warn!(
                            reason = %row.reason,
                            sender = %row.sender_address,
                            "inbound mailed REPLY refused"
                        );
                        refused.record(row);
                    }
                }
                Err(e) => tracing::error!("inbound reply merge failed: {e}"),
            }
        });
    }

    /// Build this pass's on-device INBOX spam scorer — the native twin of the web
    /// loop's `prepareSpamScoring` (`apps/fauna-web/src/lib/conversations.ts`).
    /// Fetches the caller's sealed per-user model (`fetch_spam_model`) + the
    /// admin-effective scoring policy (`get_spam_scoring_policy`), unwraps the model
    /// under the same MSEK-derived recipient key the bodies use (the model is a bare
    /// inner `wrapped_blob`, so `open_sealed_inner_record_hybrid` — NOT the two-layer
    /// inbox-feed opener; the model plaintext never leaves Rust), and constructs the
    /// shared [`InboxSpamScorer`] with the **effective** `spam_folder` threshold +
    /// Bayesian knobs (so the client's Junk line matches the MDA/nest —
    /// `mail-spam.md` § Architectural rules).
    ///
    /// `None` — a cold-start / untrained actor (`fetch_spam_model` → `None`), mail
    /// off, or any transport/unwrap failure — means "score nothing this pass":
    /// best-effort (a failure never degrades delivery), and an untrained actor is
    /// never watermarked without a basis (the [`InboxSpamScorer`] construction
    /// contract). `spam_folder_threshold == 0` (auto-Junk disabled) still builds a
    /// scorer: it is only the fallback for a message without a delivery stamp, and
    /// a user's own stamped threshold still sorts theirs (mirroring the MDA / web).
    async fn build_pass_scorer(&self, keys: &MailKeys) -> Option<InboxSpamScorer> {
        let reply = match self
            .mail_account
            .fetch_spam_model(keys.actor_id.to_vec())
            .await
        {
            Ok(reply) => reply,
            Err(e) => {
                tracing::debug!("fetch_spam_model (best-effort, no scoring this pass): {e}");
                return None;
            }
        };
        let sealed = match reply.blob {
            Some(sealed) => sealed.into_vec(),
            // Untrained actor → cold start, no scorer (no un-based watermarking).
            None => return None,
        };
        let policy = match self.mail_account.get_spam_scoring_policy().await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("get_spam_scoring_policy (best-effort, no scoring this pass): {e}");
                return None;
            }
        };
        // The model is sealed to the CURRENT generation's keypair (the client
        // re-seals it on every training write), so only `standing[0]` applies.
        let current = keys.standing.first()?;
        let mlkem_dk = current.mlkem_dk.as_deref()?;
        let model_bytes = match fauna_mail::open_sealed_inner_record_hybrid(
            &sealed,
            &current.x25519_secret,
            mlkem_dk,
        ) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::debug!("unwrap spam model (best-effort, no scoring this pass): {e}");
                return None;
            }
        };
        // A client-sealed stored model rides the published deployment baseline
        // on the reply for the AGENT to fold (the nest cannot; a plaintext
        // stored model was already folded nest-side and carries no `baseline`
        // — the no-double-fold rule, `mail-spam.md` § Encrypted-mode
        // interaction). Same fade math as the nest, so scores stay
        // byte-identical across positions.
        let model_bytes = fauna_mail::spam::fold_spam_model_baseline(
            model_bytes,
            reply.baseline.map(|b| b.into_vec()).unwrap_or_default(),
            policy.bayesian_full_confidence_samples,
        );
        let knobs = BayesianKnobs {
            bayesian_weight_milli: policy.bayesian_weight_milli,
            min_samples: policy.bayesian_min_samples,
            full_confidence_samples: policy.bayesian_full_confidence_samples,
        };
        Some(InboxSpamScorer::new(
            model_bytes,
            policy.spam_folder_threshold,
            knobs,
        ))
    }

    /// One page of decrypted records with `uid > after_uid` — the body of both
    /// [`InboundMailSource::fetch`] (`delivery = true`) and
    /// [`InboundMailSource::fetch_one`] (`delivery = false`). A delivery runs the
    /// INBOX-only work a message gets once, as it arrives: the iTIP-REPLY route
    /// to the calendar layer and the pass's spam scorer. A re-read (the
    /// attachment store refilling an evicted attachment) is not a delivery, so it
    /// resolves and opens the record identically and does neither.
    async fn fetch_page(
        &self,
        after_uid: u32,
        limit: u32,
        delivery: bool,
    ) -> Result<InboundMailPage, String> {
        // Mail not configured yet → no-op page (the receive loop idles on this rail
        // until the user enables mail; the cache stays `None` so the next tick
        // retries the derive).
        let Some(keys) = self.keys().await else {
            return Ok(InboundMailPage::default());
        };
        let reply = match self.mailbox {
            MailFeed::Inbox => self.email.inbox_fetch(after_uid, limit).await,
            MailFeed::Sent => self.email.sent_fetch(after_uid, limit).await,
        }
        .map_err(|e| e.to_string())?;
        // Only `INBOX`'s modseq is the flag-change baseline; the `Sent` feed's
        // numbers another mailbox.
        let highest_modseq = match self.mailbox {
            MailFeed::Inbox => reply.highest_modseq,
            MailFeed::Sent => 0,
        };
        let mut keys = keys;
        // A miss under the cached key set earns ONE re-derive from the current
        // mail custody per page (`MailKeyCache::refresh`), so a key change made
        // while this session runs is picked up before any record is declared
        // unopenable; a second miss under the fresh set is the record's fault.
        let mut refreshed = false;
        let mut records = Vec::with_capacity(reply.messages.len());
        let mut skipped = Vec::new();
        for m in reply.messages {
            // A message whose stored *outer* envelope exceeds the 2 MiB frame
            // cannot ride the feed inline: the reply carries an empty
            // `sealed_envelope` plus a `body_ref`, and the bytes wait on the byte
            // plane. Fetch them back into the exact stored envelope first — from
            // there the record opens identically to an inline one, which is the
            // whole point of the reference leg (only the feed's *transport*
            // moves; smtp-server.md § Message size limits). Below the frame,
            // `body_ref` is absent and nothing here changes.
            //
            // Resolve failure is fatal to the page for the same reason an open
            // failure is (below), and is transient by construction: the nest
            // re-stages the chunks on every serve, so the next tick retries.
            let sealed_envelope = match &m.body_ref {
                Some(r) => fauna_mail::body_ref::resolve_referenced_mail_body(
                    &NestPublicChunkFetcher::new(&self.nest),
                    &r.chunk_hashes,
                    r.total_bytes,
                )
                .await
                .map_err(|e| format!("resolve referenced body (uid {}): {e}", m.uid))?,
                None => m.sealed_envelope,
            };
            // One shared call does both decrypt layers (outer segment-envelope
            // decode + inner HPKE open), opening either suite under the complete
            // standing key set (current + grace) and the epoch chain. A record
            // that does not open is NOT fatal to the page: the miss is
            // deterministic for this key set (the uid and segment id the cursor
            // and dedup run on are feed metadata, untouched by the seal), so the
            // page carries it as a skip — the cursor moves past it, later mail
            // keeps arriving, and the manager tells the user
            // (`mail-app-surface.md` § Inbound client receive → *Unopenable
            // records*). Only the resolve above stays fatal, being transient.
            // Classify the seal epoch from the record's SEAL INSTANT (`stored_at`),
            // never `internal_date` (imported mail diverges by design) nor the sender-supplied `Date:` header. The unknown `0` is a
            // standing-sealed record, which the chain's standing arm opens.
            let seal_instant = m.stored_at.max(0) as u64;
            let mut opened = keys.open_record(&sealed_envelope, seal_instant);
            if opened.is_err() && !refreshed {
                refreshed = true;
                if let Some(fresh) = self.keys.refresh().await {
                    keys = fresh;
                    opened = keys.open_record(&sealed_envelope, seal_instant);
                }
            }
            let rfc5322 = match opened {
                Ok(rfc5322) => rfc5322,
                Err(e) => {
                    skipped.push(SkippedMailRecord {
                        mailbox: self.mailbox,
                        uid: m.uid,
                        reason: e.to_string(),
                    });
                    continue;
                }
            };
            // INBOX-only post-decrypt work (Sent is never REPLY-merged nor scored),
            // and only on a delivery — a re-read must not merge a REPLY twice or
            // score outside the pass `begin_pass` built its scorer for.
            let suppress_from_view = if delivery && self.mailbox == MailFeed::Inbox {
                // Score-at-ingest FIRST: feed the just-decrypted body to this
                // pass's scorer (built in `begin_pass`; `None` = cold start / mail
                // off). `observe` skips an already-`$FaunaSpamScored`
                // message and returns the junk verdict; a junk verdict is kept OUT
                // of the thread view (moving INBOX→Junk this pass — the accumulated
                // (scored, junk) UIDs flush once in `end_pass`). No `.await` here, so
                // the scorer lock is never held across one (mail-spam.md § Re-file
                // timing; the native twin of `WasmConversationsManager`).
                let junk = match self.spam_scorer.lock().unwrap().as_mut() {
                    Some(scorer) => {
                        let text = String::from_utf8_lossy(&rfc5322);
                        scorer.observe(m.uid, &m.flags, &text)
                    }
                    None => false,
                };
                // Then route an inbound iTIP REPLY to the calendar layer — never
                // from a copy the scorer just filed Junk (caldav-server.md
                // § Server-side auto-schedule, invitation rule 3).
                if !junk {
                    self.spawn_reply_merge(&rfc5322, &keys);
                }
                junk
            } else {
                false
            };
            records.push(InboundMailRecord {
                uid: m.uid,
                message_id: m.message_id,
                // The feed's `internal_date` is epoch **seconds**; the rail wants ms.
                internal_date_ms: m.internal_date.saturating_mul(1000),
                rfc5322,
                suppress_from_view,
                has_seen_flag: fauna_conversations::carries_seen_flag(&m.flags),
                mailbox: self.mailbox,
            });
        }
        Ok(InboundMailPage {
            records,
            more: reply.more,
            skipped,
            highest_modseq,
        })
    }
}

/// A mail read-state call's error: every nest serves the kinds, so any
/// refusal is worth a retry. (The "unknown kind ⇒ a nest that predates it,
/// stop silently" arm left with the compat-remnant sweep —
/// `version-compatibility.md` § Dimension 2.)
fn mail_flag_call_error<E: RpcErrorClass + std::fmt::Display>(
    e: E,
) -> fauna_conversations::MailFlagCallError {
    fauna_conversations::MailFlagCallError::Failed(e.to_string())
}

/// `fauna.email.inbox.mark_seen` as the shared mail read-state sync calls it
/// ([`fauna_conversations::backends::smtp::sync_mail_read_state`]) — one door
/// for both the native `INBOX` source and the web manager's.
pub async fn inbox_mark_seen_call<R>(
    email: &EmailClient<R>,
    uids: Vec<u32>,
) -> Result<(), fauna_conversations::MailFlagCallError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    email
        .inbox_mark_seen(uids)
        .await
        .map(|_| ())
        .map_err(mail_flag_call_error)
}

/// `fauna.email.inbox.flag_changes`, each change reduced to whether it
/// carries `\Seen` — the other half of [`inbox_mark_seen_call`].
pub async fn inbox_flag_changes_call<R>(
    email: &EmailClient<R>,
    since_modseq: u64,
    after_uid: u32,
    limit: u32,
) -> Result<fauna_conversations::MailFlagChangesPage, fauna_conversations::MailFlagCallError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let reply = email
        .inbox_flag_changes(since_modseq, after_uid, limit)
        .await
        .map_err(mail_flag_call_error)?;
    Ok(fauna_conversations::MailFlagChangesPage {
        changes: reply
            .changes
            .iter()
            .map(|c| fauna_conversations::MailFlagChange {
                uid: c.uid,
                modseq: c.modseq,
                has_seen_flag: fauna_conversations::carries_seen_flag(&c.flags),
            })
            .collect(),
        highest_modseq: reply.highest_modseq,
        more: reply.more,
    })
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl InboundMailSource for NestMailInboundSource {
    async fn fetch(&self, after_uid: u32, limit: u32) -> Result<InboundMailPage, String> {
        self.fetch_page(after_uid, limit, true).await
    }

    /// `fauna.email.inbox.mark_seen` — the `INBOX` source only; the `Sent`
    /// feed's messages are own and never written.
    async fn mark_seen(
        &self,
        uids: Vec<u32>,
    ) -> Result<(), fauna_conversations::MailFlagCallError> {
        if self.mailbox != MailFeed::Inbox {
            return Err(fauna_conversations::MailFlagCallError::Unsupported);
        }
        inbox_mark_seen_call(&self.email, uids).await
    }

    /// `fauna.email.inbox.flag_changes`, each change reduced to whether it
    /// carries `\Seen` — the `INBOX` source only.
    async fn flag_changes(
        &self,
        since_modseq: u64,
        after_uid: u32,
        limit: u32,
    ) -> Result<fauna_conversations::MailFlagChangesPage, fauna_conversations::MailFlagCallError>
    {
        if self.mailbox != MailFeed::Inbox {
            return Err(fauna_conversations::MailFlagCallError::Unsupported);
        }
        inbox_flag_changes_call(&self.email, since_modseq, after_uid, limit).await
    }

    /// The seam's re-read over the same page read [`Self::fetch`] makes, minus
    /// the delivery-only INBOX work ([`NestMailInboundSource::fetch_page`]): the
    /// attachment store asking for one record's MIME again is not mail arriving.
    async fn fetch_one(&self, uid: u32) -> Result<Option<InboundMailRecord>, String> {
        let Some(after_uid) = uid.checked_sub(1) else {
            return Ok(None);
        };
        let page = self.fetch_page(after_uid, 1, false).await?;
        Ok(page.records.into_iter().find(|r| r.uid == uid))
    }

    /// Refresh this pass's INBOX spam scorer before the page loop — the native twin
    /// of the web loop's `prepareSpamScoring`. Only the INBOX feed is per-user-scored
    /// (Sent mail is never scored — `mail-spam.md` § the score-at-ingest flow); a
    /// fresh scorer each pass picks up fresh training + admin-policy changes.
    /// Best-effort: any failure leaves the scorer `None` (score nothing this pass),
    /// never aborts the receive loop.
    async fn begin_pass(&self) {
        if self.mailbox != MailFeed::Inbox {
            return;
        }
        // Mail not enabled yet → no keys → no scorer (the `fetch` is a no-op page
        // anyway); the cache stays `None` so the next tick retries the derive.
        let scorer = match self.keys().await {
            Some(keys) => self.build_pass_scorer(&keys).await,
            None => None,
        };
        *self.spam_scorer.lock().unwrap() = scorer;
    }

    /// Flush this pass's accumulated on-device disposition in ONE
    /// `apply_spam_disposition` (watermark every scored UID + move the junk subset
    /// INBOX→Junk) — the native twin of the web loop's `flushSpamScoring`. Runs even
    /// when a `fetch` errored mid-pass (see `poll_inbound_mail`), so a junk verdict
    /// already suppressed from the view is still watermarked + moved. Clears the
    /// scorer so an errored pass can't leak into the next (rebuilt in `begin_pass`).
    /// Best-effort — a failure never degrades delivery (the un-watermarked messages
    /// are simply re-scored next pass).
    async fn end_pass(&self) {
        if self.mailbox != MailFeed::Inbox {
            return;
        }
        let disposition = {
            let mut guard = self.spam_scorer.lock().unwrap();
            guard.take().map(|mut scorer| scorer.take())
        };
        let Some((scored_uids, junk_uids)) = disposition else {
            return;
        };
        if scored_uids.is_empty() {
            return;
        }
        if let Err(e) = self
            .email
            .apply_spam_disposition(scored_uids, junk_uids)
            .await
        {
            tracing::debug!("apply_spam_disposition (best-effort): {e}");
        }
    }
}

/// Derive the per-actor mail key material (recipient HPKE secret + MSEK + actor
/// id) from the account's mail custody — `None` when not ready: no identity
/// keypair, a transient custody read failure, or mail/calendar not provisioned
/// (no MSEK). Shared by [`NestMailInboundSource::keys`] (the
/// inbound-mail decrypt) and [`NestSchedulingSink::keys`] (the scheduling-iMIP
/// calendar apply) — both need the same MSEK + actor id (priority #2; the CalDAV
/// store seals under the SAME MSEK, per `caldav-server.md` § Authentication
/// — one shared credential serves IMAP + CalDAV). Never holds a lock across the
/// `.await`; callers cache the result themselves. The MSEK never leaves Rust.
#[cfg(not(target_arch = "wasm32"))]
async fn load_mail_keys(
    nest: &Arc<NestClient>,
    mail: &dyn fauna_client_config::MailStore,
) -> Option<MailKeys> {
    let actor_id = nest.auth().keypair()?.actor_id().0;
    let mail = mail.load().await.ok()?;
    let msek = mail.msek?;
    // The prior grace generations, capped so current + grace ==
    // SNAPSHOT_GRACE_KEYPAIRS — the same history the snapshot builder takes, so
    // the standing key set, the epoch roots and the index ring are exactly what
    // the MDA opens with.
    let mut prior_mseks: Vec<[u8; 32]> = mail
        .prior_mseks
        .iter()
        .take(SNAPSHOT_GRACE_KEYPAIRS - 1)
        .map(|prior| **prior)
        .collect();
    let keys = MailKeys::from_custody(actor_id, &msek, &prior_mseks);
    // The local copy is not zeroize-on-drop; `keys` now carries its own.
    prior_mseks.zeroize();
    Some(keys)
}

/// The nest-backed calendar-apply sink for the **mailbox-less CalDAV iMIP rail**
/// — the receive counterpart of the scheduling sender, shared by every native
/// app (linux directly; macOS / iOS / Windows / Android via the `fauna-ffi`
/// `conversations_session` factory). Registered on the session via
/// [`ConversationsSession::register_scheduling_sink`](fauna_conversations::session::ConversationsSession::register_scheduling_sink);
/// the receive loop hands it each decrypted scheduling iMIP (raw RFC 5322) and it
/// routes by METHOD to the recipient's calendar via the shared
/// [`CalDavClient::apply_inbound_scheduling_from_message`] (REQUEST → materialize,
/// CANCEL → tombstone, REPLY → merge an RSVP) — exactly how
/// [`NestMailInboundSource`] routes an inbound mail REPLY, but over the WS-RPC
/// rail a mailbox-less user (CalDAV on / email off) has *instead* of mail, so a
/// scheduling invite still lands on their calendar with no mailbox
/// (`docs/goal/behavior/caldav-server.md` § Server-side auto-schedule, Half-1).
///
/// The MSEK + actor id derive **lazily** from the mail custody on first
/// apply (the same `mail.msek` the CalDAV store seals under; absent → the apply is
/// a graceful no-op until the user provisions their calendar credential). The
/// MSEK never leaves Rust — the apply runs entirely in shared Rust.
#[cfg(not(target_arch = "wasm32"))]
pub struct NestSchedulingSink {
    nest: Arc<NestClient>,
    /// The shared lazily-derived key cache.
    keys: Arc<MailKeyCache>,
    /// The session's remembered succession answers — one dial per bound
    /// organizer identity per session (`caldav-server.md` § Who may mutate an
    /// existing event over the inbound rail → *A succeeded organizer*). Lives
    /// here because the sink lives as long as the receive session; the
    /// resolver around it is built per message.
    successions: fauna_client_caldav::SuccessionMemo,
    /// The session's manager, whose peer-anchor store holds the chain head the
    /// organizer-succession walk is handed ([`NativeOrganizerSuccessionDialer`]).
    /// `Weak`: the sink is registered on the session that owns the manager.
    manager: Weak<fauna_conversations::ConversationsManager>,
    /// Where a refusal is recorded — the session manager's inbox
    /// ([`fauna_conversations::refused_changes`]), which reaches the account
    /// plane once the host's account store is up.
    refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestSchedulingSink {
    /// `manager` is the receiving session's own (`Arc::downgrade`) — its
    /// peer-anchor store is where a succeeded organizer's held head rests;
    /// `refused` is its refused-change inbox
    /// (`ConversationsManager::refused_changes`).
    pub fn new(
        nest: Arc<NestClient>,
        mail: Arc<dyn fauna_client_config::MailStore>,
        manager: Weak<fauna_conversations::ConversationsManager>,
        refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
    ) -> Self {
        let keys = MailKeyCache::new(Arc::clone(&nest), mail);
        Self::over(nest, keys, manager, refused)
    }

    /// [`Self::new`] over a caller-supplied cache, so a client wiring several
    /// mail-keyed consumers pays one custody read for all of them.
    pub fn over(
        nest: Arc<NestClient>,
        keys: Arc<MailKeyCache>,
        manager: Weak<fauna_conversations::ConversationsManager>,
        refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
    ) -> Self {
        Self {
            nest,
            keys,
            successions: fauna_client_caldav::SuccessionMemo::new(),
            manager,
            refused,
        }
    }

    /// This sink's keys — `None` = not ready (retry next drain); see
    /// [`MailKeyCache::get`].
    async fn keys(&self) -> Option<MailKeys> {
        self.keys.get().await
    }

    /// Record the user-facing notice of a refusal
    /// (`inbound-scheduling-authority.md` § *Surfacing*), so the Events page
    /// can tell the user someone tried to change their calendar.
    ///
    /// **Never fails the drain, and never retries the apply.** The security
    /// property — the stored event is untouched — is already achieved by the
    /// time this runs; the notice goes to the session's refused-change inbox,
    /// which never blocks and holds it until the account store is up.
    ///
    /// The sink composes nothing: the row is
    /// [`SchedulingApplyOutcome::refused_change_record`]'s, and the record is
    /// the account plane's (`fauna.state.refused-scheduling-changes`).
    async fn record_refusal(
        &self,
        outcome: &fauna_client_caldav::SchedulingApplyOutcome,
        method: &str,
        origin: &fauna_client_caldav::InboundOrigin,
    ) {
        let Some(row) = outcome.refused_change_record(method, origin, now_secs()) else {
            return;
        };
        self.refused.record(row);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl SchedulingSink for NestSchedulingSink {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        origin: fauna_conversations::backend::SchedulingOrigin,
    ) -> Result<(), String> {
        // Not provisioned yet (no MSEK) → no calendar store to apply to; a graceful
        // no-op the loop retries next drain (the user may provision later), exactly
        // like the inbound-mail source idles until `mail.msek` exists.
        let Some(keys) = self.keys().await else {
            return Ok(());
        };
        // The refused-change row names the METHOD, and the shared apply routes
        // by it internally — so read it here, from the same bytes, rather than
        // widening the outcome to carry it back out.
        let method = fauna_client_caldav::scheduling_method(&raw_rfc5322);
        let inbound_origin = fauna_client_caldav::InboundOrigin {
            author: origin.author,
            home_nest_url: origin.home_nest_url,
        };
        let outcome = CalDavClient::new(Arc::clone(&self.nest))
            .apply_inbound_scheduling_from_message(
                &keys.actor_id,
                &keys.msek,
                &keys.prior_mseks,
                &raw_rfc5322,
                now_secs(),
                // Pass-through only: who may create / change / cancel is decided
                // once, in the shared apply (`caldav-server.md` § Who may mutate
                // an existing event over the inbound rail).
                &inbound_origin,
                &fauna_client_caldav::MemoizedSuccessionResolver {
                    addresses: fauna_client_caldav::DiscoveryPrincipalResolver {
                        discovery: fauna_client_caldav::AnonAttendeeDiscovery,
                        own_nest_url: self.nest.nest_url(),
                    },
                    dialer: NativeOrganizerSuccessionDialer {
                        manager: Weak::clone(&self.manager),
                    },
                    own_nest_url: self.nest.nest_url(),
                    memo: self.successions.clone(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        if outcome.is_refused() {
            // Someone tried to change this calendar and was not allowed to —
            // worth a line an operator of their own box can find.
            tracing::warn!(?outcome, "refused inbound scheduling iMIP");
            self.record_refusal(&outcome, &method, &inbound_origin)
                .await;
        } else {
            tracing::debug!(?outcome, "applied inbound scheduling iMIP");
        }
        Ok(())
    }
}

/// The native succession dial of the inbound-mutation rule — what
/// [`NestSchedulingSink`] hands the shared
/// [`MemoizedSuccessionResolver`](fauna_client_caldav::MemoizedSuccessionResolver),
/// so every native app (tui and linux directly, the four UniFFI apps through
/// the `fauna-ffi` session factory) resolves identically. Addresses go to the
/// shared anon `by_handle` discovery beside it; the memo, the anchor and the
/// once-per-session rule are the shared resolver's.
///
/// A **succession** is this type's one job (`caldav-server.md` § Who may mutate
/// an existing event over the inbound rail → *A succeeded organizer*): the
/// verified walk `fauna_client_recovery` owns, dialed anonymously at the nest
/// the event was **bound** to, inside that crate's round-trip budget, so an
/// unreachable bound nest costs the inbound drain a bounded wait once per
/// session and then the refusal that was already standing.
#[cfg(not(target_arch = "wasm32"))]
struct NativeOrganizerSuccessionDialer {
    /// The receiving session's manager: its peer-anchor store holds the
    /// account's held chain heads (`fauna.state.peer-anchors`).
    manager: Weak<fauna_conversations::ConversationsManager>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NativeOrganizerSuccessionDialer {
    /// The chain head this account already holds for `actor`, which the walk
    /// must be handed so a chain that rewrites or truncates what was seen is
    /// refused (`identity-succession.md` § The succession statement).
    /// `Ok(None)` is genuine first contact; `Err` means the anchors could not
    /// be read — the session is gone, the account store is not lent yet, or
    /// the read failed — so whether a head is held is unknown, and the caller
    /// answers *no answer* rather than quietly walking at first-contact grade.
    async fn held_chain_head(
        &self,
        actor: &fauna_core::identity::ActorId,
    ) -> Result<Option<fauna_core::recovery::ChainHead>, ()> {
        let store = self
            .manager
            .upgrade()
            .and_then(|m| m.peer_anchor_store())
            .ok_or(())?;
        let anchors = store.peer_anchors().await.map_err(|_| ())?;
        Ok(anchors.known_chain_head(actor))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl fauna_client_caldav::SuccessionDialer for NativeOrganizerSuccessionDialer {
    async fn walk(
        &self,
        old_actor_id: &str,
        anchor_nest_url: &str,
    ) -> fauna_client_caldav::SuccessionLookup {
        use fauna_client_caldav::SuccessionLookup;
        let Ok(old) = fauna_core::identity::ActorId::from_hex(old_actor_id) else {
            return SuccessionLookup::NotAsked;
        };
        let Ok(known_head) = self.held_chain_head(&old).await else {
            return SuccessionLookup::NotAsked;
        };
        use fauna_client_recovery::AnchoredWalk;
        match fauna_client_recovery::witness::walk_at_nest_url(anchor_nest_url, old, known_head)
            .await
        {
            AnchoredWalk::Succeeded(step) => {
                SuccessionLookup::Succeeded(step.new_actor_id.to_hex())
            }
            AnchoredWalk::NeverSucceeded => SuccessionLookup::NotSucceeded,
            AnchoredWalk::Unsettled => SuccessionLookup::Unproven,
        }
    }
}

// ── Content-index builder launch (`NestMailIndexLauncher`) ────────────────────

/// The native [`IndexBuilderLauncher`] — resumes this actor's mail index and
/// hands back the observer, at the one moment the ordering contract allows.
///
/// **Why it lives here and not in `fauna-client-index`.** The builder needs the
/// actor's MSEK, which lives in the mail custody and is loaded by
/// [`load_mail_keys`] in this crate. The design rule is that *the MSEK never
/// leaves Rust* (`conversations.md` § Architectural rules #2) — that is a rule
/// about app glue, not about crate boundaries, so handing 32 bytes from one
/// shared-Rust crate to another is exactly what it permits and what
/// `key-material-hierarchy.md` § Path B-sibling-4 describes (every client
/// derives it from its own mail custody). What it forbids — an
/// accessor that hands the MSEK to tui/linux glue so *they* can construct a
/// builder — is what this launcher exists to avoid: app glue passes a
/// `NestClient` and gets back an opaque observer.
///
/// It shares the [`MailKeyCache`] with the inbound-mail sources, so a login that
/// wires both pays one mail-custody load, not two.
#[cfg(not(target_arch = "wasm32"))]
pub struct NestMailIndexLauncher {
    keys: Arc<MailKeyCache>,
    /// The lease rendezvous — kept beside the rail because
    /// [`crate::index_lease::IndexLease`] observes and heartbeats over the same
    /// connection the segments publish on.
    nest: Arc<NestClient>,
    /// This device's seat at the advisory `index` lease, when app glue supplied
    /// one. `None` ⇒ this build never heartbeats and its builder stays
    /// uncoordinated — the pre-lease behaviour, and the honest answer for a seat
    /// whose device identity shared Rust cannot see (`index_lease` module docs).
    lease_seat: Option<crate::index_lease::IndexLeaseSeat>,
    /// The one `__index` rail this login publishes and reads over — shared by the
    /// build path ([`IndexBuilderLauncher::launch`]) and the query path
    /// ([`Self::local_search_index`]) rather than minted per caller. The rail is
    /// kind-agnostic and stateless over the `NestClient`, so one owner is both
    /// correct and the thing that keeps the MSEK single-sourced here.
    publisher: Arc<fauna_client_index::IndexRailPublisher>,
    /// Ends the flush debounce driver when the session goes away.
    cancel: CancellationToken,
    /// The **stable** observer container this login registers exactly once, and
    /// whose arm set grows in place.
    ///
    /// It is created here rather than in [`IndexBuilderLauncher::launch`]
    /// because an arm can attach long after launch (mail enabled mid-session),
    /// and the seam holds exactly **one** observer slot: minting a replacement
    /// would discard every already-running arm's live state — its seeded
    /// `(kind, content_id)` re-index guard and anything staged but not yet
    /// flushed. So the object handed to the manager never changes; only its
    /// contents do.
    arms: Arc<FanOutObserver>,
    /// This login's seat on the advisory `index` lease — the gate and the
    /// first-answer watch — started once by `launch` and shared by every arm,
    /// **including one that attaches later**. The lease is held by a *device*,
    /// not by a kind, so a late arm must join this seat rather than contend for
    /// a second one against its own login.
    lease: std::sync::Mutex<Option<crate::index_lease::IndexLease>>,
    /// The master arm's staging door, kept so the **contacts reconcile walk**
    /// can stage into it.
    ///
    /// Every other producer reaches its builder through the observer seam, which
    /// is why no handle was needed before. Contacts have no seam to reach: the
    /// corpus is nest-resident and externally mutated, so the producer is a walk
    /// this launcher runs itself (`content-index.md` § Ingest triggers, v1 — the
    /// contacts ruling). The walk and the arm therefore both live behind the
    /// launcher, and this is the door between them.
    ///
    /// ⚠ A [`fauna_client_index::DirectStager`] rather than the raw
    /// `Arc<IndexBuilder>` it used to be: the builder alone stages without
    /// pulsing the flush driver, so the corpus sat until the 60-second backstop
    /// with nothing anywhere reporting it. The stager's type doc carries the
    /// full account.
    master_builder: std::sync::Mutex<Option<Arc<fauna_client_index::DirectStager>>>,
    /// The shared-folder key resolver, when app glue wired one — what lets the
    /// File arm render the sealed names of sets shared **with** this actor.
    ///
    /// Injected rather than constructed here, and that is a dependency fact
    /// rather than a preference: the production resolver lives in
    /// `fauna-client-folders`, which depends on *this* crate under its `mls`
    /// feature, so the edge cannot run the other way. The trait itself is in
    /// `fauna-core`, which is why the seam can exist at all.
    ///
    /// **`None` is a supported state, not a stub** — exactly like `lease_seat`.
    /// Without it the arm still indexes every set this actor *owns* (the owner
    /// root is derived in-crate from the identity seed), and a shared set's rows
    /// are skipped **without burning the re-index guard**, so the walk on a seat
    /// that does hold the keys stages them.
    folder_keys: std::sync::Mutex<Option<Arc<dyn fauna_core::folder_keys::FolderKeyResolver>>>,
    /// The account's attested predecessor ids
    /// (`AccountRegistry::attested_predecessor_actor_ids`), injected beside
    /// [`Self::folder_keys`] for the same reason: the File arm's reader seat
    /// admits a row a retired identity signed without a succession lookup
    /// (writer-signed change records, ruling (8)(b) source (ii)). Empty is a
    /// supported state — the seat falls back on the statement walk — and the
    /// state of android, apple and windows until their hosts pass it.
    predecessors: std::sync::Mutex<Vec<[u8; 32]>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestMailIndexLauncher {
    /// `lease_seat` puts this login's builder under the advisory `index` lease
    /// (`participants.md` § Coordination primitive → *The `index` kind under the
    /// lease*). Pass `Some` wherever app glue can name this device;
    /// **`None` is a supported state, not a stub** — the builder then runs
    /// uncoordinated exactly as it did before the lease existed, which is what
    /// keeps the heartbeat additive across a mixed fleet.
    pub fn new(
        nest: Arc<NestClient>,
        keys: Arc<MailKeyCache>,
        lease_seat: Option<crate::index_lease::IndexLeaseSeat>,
    ) -> Arc<Self> {
        Arc::new(Self {
            keys,
            nest: Arc::clone(&nest),
            lease_seat,
            publisher: Arc::new(fauna_client_index::IndexRailPublisher::new(nest)),
            cancel: CancellationToken::new(),
            arms: Arc::new(FanOutObserver::default()),
            lease: std::sync::Mutex::new(None),
            master_builder: std::sync::Mutex::new(None),
            folder_keys: std::sync::Mutex::new(None),
            predecessors: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Hand the File arm's reader seat the account's attested predecessor ids.
    /// Late wiring is fine, like [`Self::set_folder_key_resolver`]: the walk
    /// re-reads the slot on every sweep. Empty clears it.
    pub fn set_attested_predecessors(&self, predecessors: Vec<[u8; 32]>) {
        *self.predecessors.lock().unwrap() = predecessors;
    }

    /// Wire the shared-folder key resolver, so the File arm can render the
    /// sealed names of sets shared **with** this actor
    /// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
    /// SCOPED*: group-shared sets are included).
    ///
    /// A setter rather than a `new` parameter for the reason
    /// `SnapshotsClient::with_label_custody` is one: the launcher is constructed
    /// at several seats, the resolver needs a crate this one cannot depend on
    /// (see [`Self::folder_keys`]), and a seat that has not built one is a
    /// supported state rather than a broken one. Late wiring is fine — the walk
    /// re-reads the slot on every sweep, so a resolver installed after login
    /// takes effect at the next one, the same attach-later rule the rest of this
    /// launcher follows.
    ///
    /// Glue passes an opaque resolver, never key material — the posture every
    /// other seam here holds.
    pub fn set_folder_key_resolver(
        &self,
        resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver>,
    ) {
        *self.folder_keys.lock().unwrap() = Some(resolver);
    }

    /// The Search page's **local arm** for this login, or `None` when there is
    /// no mail to index (no `mail.msek`).
    ///
    /// The query-side twin of [`IndexBuilderLauncher::launch`], and the reason
    /// both live on one object: this is the only place that holds the MSEK, so
    /// it is the only place that can open the sealed slice. App glue passes the
    /// content lookup its conversations store answers and gets back an opaque
    /// `LocalSearchIndex` — never a key (`conversations.md` § Architectural
    /// rules #2, same posture as `launch`).
    ///
    /// `None` is a normal state the Search page renders as *no local rows*,
    /// never an error (`content-index.md` § Where queries run) — matching
    /// `launch`'s `Option` for the same reason.
    pub async fn local_search_index(
        &self,
        lookup: Arc<dyn fauna_client_index::MailContentLookup>,
    ) -> Option<Arc<dyn fauna_client_search::LocalSearchIndex>> {
        // Two readers, one arm — the two key classes have two manifests and two
        // keys, and the manager holds exactly one slot
        // (`fauna_client_search::CompositeLocalSearch` carries the reasoning).
        //
        // **Each class is independently optional, and that is the point.** The
        // mail reader needs an MSEK that does not exist until mail is enabled,
        // while the master reader needs only the identity seed every logged-in
        // actor has. Before this, the single slot was filled with the mail
        // reader alone, so a user without mail had *no* local arm at all and a
        // user with mail had one that answered `None` for every master kind —
        // either way the Conversation kind built segments nothing could read.
        let mut members: Vec<Arc<dyn fauna_client_search::LocalSearchIndex>> = Vec::new();
        // Whether the MSEK-riding half (the mail reader, the `Contact` claim) is
        // in this arm. Without it the arm is still real — the master reader's
        // classes answer — but it is marked as awaiting its precondition, so the
        // manager re-mints it on a later query once mail is enabled instead of
        // caching a contact-blind arm for the life of the process.
        let mut msek_ready = false;
        if let Some(keys) = self.keys.get().await {
            msek_ready = true;
            // Derive the ring HERE, where the MSEK already is, and hand the arm
            // the ring — never the root. The borrowed `keys` is `ZeroizeOnDrop`
            // and dies with this block; the arm that outlives it holds only
            // zeroize-on-drop segment keys (`key-material-hierarchy.md`
            // § Carrier shape; the same posture `NestContactCorpus` took for the
            // contacts arm).
            members.push(Arc::new(fauna_client_index::MailLocalSearch::new(
                keys.index_ring(),
                Arc::clone(&self.publisher),
                Arc::clone(&lookup),
            )));
        }
        if let Some(master_key) = self.master_index_key() {
            let mut master = fauna_client_index::MasterLocalSearch::new(
                master_key,
                Arc::clone(&self.publisher),
                lookup,
            )
            // The posts resolver needs only the connection, so it attaches
            // unconditionally — which is what makes the reader claim `Post`
            // at all (the claim-nothing-you-cannot-project rule).
            .with_posts(Arc::new(NestPostCorpus {
                nest: Arc::clone(&self.nest),
            }))
            // The files resolver needs the same thing the walk does — the
            // identity seed for the owner root, plus whatever shared-set resolver
            // glue wired — so it attaches unconditionally like posts. Its own
            // read answers *no rows* when this login has no keypair, which is the
            // no-reader posture rather than an unclaimed kind.
            .with_files(Arc::new(NestFileCorpus {
                nest: Arc::clone(&self.nest),
                resolver: std::sync::Mutex::new(self.folder_keys.lock().unwrap().clone()),
                predecessors: self.predecessors.lock().unwrap().clone(),
            }));
            // The contacts resolver rides the MSEK, not the master key — see
            // `MasterLocalSearch::contacts`. Attaching it is what makes the
            // reader claim `Contact` at all, so a mail-less seat's contact
            // segments (of which it has none) stay unclaimed rather than
            // producing hits nothing can project.
            //
            // The gate reads the cache but keeps nothing from it: the resolver
            // carries the cache *handle* and asks per read. So this is a readiness question
            // ("is there an MSEK, i.e. may this reader claim the kind"), not a
            // key fetch, and the `MailKeys` it borrows drops at the `if`.
            if msek_ready {
                master = master.with_contacts(Arc::new(NestContactCorpus {
                    nest: Arc::clone(&self.nest),
                    keys: Arc::clone(&self.keys),
                }));
            }
            members.push(Arc::new(master));
        }
        if members.is_empty() {
            // No class is openable yet. `None` keeps the resolver's re-ask
            // contract: the manager caches only a `Some`, so the next query
            // asks again once a key exists.
            return None;
        }
        let arm = fauna_client_search::CompositeLocalSearch::new(members);
        Some(Arc::new(if msek_ready {
            arm
        } else {
            arm.awaiting_precondition()
        }))
    }
}

/// Dropping the launcher ends its flush driver — a re-login builds a fresh
/// session and launcher, and the old driver must not keep publishing against the
/// previous actor's builder.
#[cfg(not(target_arch = "wasm32"))]
impl Drop for NestMailIndexLauncher {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl IndexBuilderLauncher for NestMailIndexLauncher {
    async fn launch(&self) -> Option<Arc<dyn MessageIndexObserver>> {
        // Start contending for the advisory `index` lease before any builder
        // exists, because the gate has to be attached at construction. `start`
        // only spawns — it never awaits a round trip — so its observe goes out
        // now and lands while the arm resumes below do their own round trips.
        //
        // **A gate that has not yet had its first answer reads *closed*, and
        // the launch walks are offered ONCE.** The Conversation attach walk
        // runs the instant this function returns and the mailbox re-page on
        // the first sweep after it; a closed gate withholds both, unrecorded,
        // and nothing re-offers them until the next launch — so on the seat
        // that turns out to WIN, a whole session of restored history would be
        // unsearchable. Hence the wait below, after the resumes: the walks are
        // never offered to an undecided gate (`content-index.md` § Where the
        // index is built → *The builder and the advisory task lease*, the
        // launch-walk sub-bullet; pinned by
        // `tests/index_lease_first_decision_tests.rs`). Erring closed past the
        // ceiling stays the right direction — the cost is a re-walk next
        // launch, where erring open is the N× republish the lease exists to
        // prevent.
        //
        // **One gate for both arms.** The lease is held by a *device*, not by a
        // kind, so the two builders share the seat this login contends for
        // (`participants.md` § Coordination primitive → *The `index` kind under
        // the lease*). Two independent contenders on one device would fight
        // each other for their own seat. It is stored rather than kept local
        // because an arm attaching later joins this same seat.
        let lease = self.lease_seat.as_ref().map(|seat| {
            crate::index_lease::start(Arc::clone(&self.nest), seat, self.cancel.clone())
        });
        *self.lease.lock().unwrap() = lease;

        // The two key classes launch **independently**, and that independence is
        // the point: mail rides the MSEK (absent until the user provisions
        // mail), the master class rides the identity seed (present for any
        // logged-in actor). Before S4 a missing MSEK meant no index at all —
        // which would now silently cost a mail-less user their *conversation*
        // search, a class whose key has nothing to do with mail.
        //
        // Neither answer is final. An arm that cannot resume now — mail not
        // enabled yet, or a rail read that failed transiently — is retried by
        // the receive loop on every sweep of its kind
        // ([`IndexBuilderLauncher::ensure_arm`]), which is why this returns the
        // container unconditionally instead of `None`: `None` would promise the
        // loop that no observer can ever appear, and that is exactly the
        // one-shot behaviour that left mail enabled mid-session unsearchable for
        // the life of the process.
        self.ensure_mail_arm().await;
        self.ensure_master_arm().await;
        // The launch's first offer waits for the lease's first answer — see the
        // lease block above. Unconditional rather than "only if an arm
        // resumed": the rule "a seated launch never returns to an undecided
        // gate" is the one a cold reader can hold, and a launch with no arm
        // pays it only when the nest is not answering, which is also why it has
        // no arm. A late arm gets the same wait in `ensure_arm`.
        self.await_lease_first_answer().await;
        // The third-class corpora have no receive loop to re-present them, so
        // the walks at attach are the only thing that indexes them this launch.
        self.run_contacts_walk().await;
        self.run_posts_walk().await;
        self.run_files_walk().await;

        Some(Arc::clone(&self.arms) as Arc<dyn MessageIndexObserver>)
    }

    /// Routed by the kind's **class**, because a class is what an arm covers: a
    /// sweep of any master kind asks for the one master arm, which stages every
    /// master kind this seat builds. The first such sweep attaches it; later
    /// ones find the class claimed and answer `false`, which is the correct
    /// "nothing newly attached".
    async fn ensure_arm(&self, kind: IndexableKind) -> bool {
        let attached = match kind {
            IndexableKind::Mail => self.ensure_mail_arm().await,
            IndexableKind::Conversation => self.ensure_master_arm().await,
        };
        // A late arm's first offer is the same once-only backlog a launch-time
        // arm's is (the loop reopens the window and re-walks / re-pages on
        // `true`), so it waits for the lease's first answer exactly as `launch`
        // does. Free after the first sweep: the watch already reads settled.
        if attached {
            self.await_lease_first_answer().await;
        }
        // **Attach-time coverage, per launch — the contacts arm's correctness
        // carrier** (`content-index.md` § Ingest triggers, v1: *the walk is the
        // correctness carrier; the push is best-effort freshness*, so the arm is
        // buildable walk-first, before the nest emits the change event).
        //
        // Run on every sweep rather than only on the sweep that attached, and
        // that is not a spare re-run: the walk is gated on the MSEK while the
        // master arm is not, so on a seat that enables mail mid-session the
        // attaching sweep finds no MSEK and a later one does. The ctag precheck
        // makes each unchanged re-run one `list_addressbooks` and no publish.
        self.run_contacts_walk().await;
        // The posts walk rides the same sweeps: no push event exists for posts,
        // so attach + sweep cadence IS its freshness (another seat's post is
        // found on the next sweep). The marker precheck makes an unchanged
        // corpus one `fauna.posts.list` page and no publish.
        self.run_posts_walk().await;
        // The files walk rides the same sweeps, and here the cadence is not the
        // only freshness carrier: `fauna.sync.changed` nudges the walk the moment
        // any participant records a change. The sweep run is what covers a
        // dropped or missed nudge — the walk is the correctness carrier, doctrine
        // verbatim. An unchanged corpus costs a paged drain and no publish.
        self.run_files_walk().await;
        attached
    }

    /// The freshness half of the contacts arm: a card written through the MDA
    /// while this app runs reaches the Search page without a relaunch.
    ///
    /// The whole body is the same walk `launch` and `ensure_arm` run, and that
    /// is the design rather than a shortcut — the walk is idempotent and
    /// ctag-suppressed, so re-running it is the *only* correct response to "the
    /// corpus moved" and a duplicated or spurious event costs one
    /// `list_addressbooks` with no publish. Nothing here may become the sole
    /// carrier of a change: `content-index.md` § Ingest triggers, v1 makes the
    /// attach/sweep walk the correctness carrier precisely so a dropped push —
    /// the ordinary fate of a transient broadcast — costs latency only.
    async fn corpus_changed(&self, corpus: fauna_conversations::backend::NestCorpus) {
        match corpus {
            fauna_conversations::backend::NestCorpus::AddressBook => {
                self.run_contacts_walk().await;
            }
            fauna_conversations::backend::NestCorpus::Files => {
                self.run_files_walk().await;
            }
        }
    }
}

/// The production [`fauna_client_index::ContactCorpusRead`] — the query-time
/// wire read that resolves contact hits (`content-index.md` § Where queries run).
///
/// Unsealing a card body needs the MSEK, so this holds the shared
/// [`MailKeyCache`] **handle** and asks it per read — it does not hold the key.
/// App glue still only ever hands over a `NestClient` and gets back an opaque
/// index (`conversations.md` § Architectural rules #2); what changed is that the
/// never-a-key posture now holds *inside* Rust too.
///
/// **Why a handle rather than a copy** (`key-material-hierarchy.md` § Carrier
/// shape). This used to be
/// `{ nest, msek: [u8; 32], actor_id: [u8; 32] }`, minted by copying the raw key
/// out of the cache — `msek: keys.msek`. That bare array is `Copy`, so it
/// duplicated silently on every assignment and pass-by-value, none of the
/// duplicates were ever zeroized, and the whole struct was held for the entire
/// login session behind `MasterLocalSearch::contacts` and freed in the clear.
/// The cache's own doc already said why that is wrong — *"this is why the MSEK
/// never leaves Rust: consumers hold an `Arc` of the cache and ask it for keys
/// inside shared Rust"* ([`MailKeyCache`]) — and the walk had always used this
/// posture. Now both halves of the contacts arm do, so there is no second
/// custody type to keep in step (priority #4: fix the family, not one member).
/// The [`MailKeys`] a read borrows is `ZeroizeOnDrop` and lives only as long as
/// the `read_corpus` call that asked for it.
#[cfg(not(target_arch = "wasm32"))]
struct NestContactCorpus {
    nest: Arc<NestClient>,
    keys: Arc<MailKeyCache>,
}

/// Compile-time custody pin for [`NestContactCorpus`] — two pointers wide, which
/// is to say **no inline key material**.
///
/// A runtime test cannot observe an absent field, and the carrier rule's usual
/// pin does not apply here: `ZeroizeOnDrop`'s destructor is what pins a type that
/// *holds* key material (`key-material-hierarchy.md` § Carrier shape → *Pinned at
/// compile time*), whereas what needs pinning here is that this type holds
/// **none**. Reintroducing the `msek: [u8; 32]` this fix removed — the single
/// most likely regression, since copying the key out of the cache reads as the
/// obvious way to reach it — grows the struct by 32 bytes and fails this
/// assertion at the pin rather than silently restoring the finding.
///
/// A legitimately new *non-key* field updates the expected width in the same
/// commit; treat that edit as the prompt to re-read the rule above, which is the
/// only job this pin has.
#[cfg(not(target_arch = "wasm32"))]
const _NEST_CONTACT_CORPUS_HOLDS_NO_BARE_KEY: () = {
    assert!(std::mem::size_of::<NestContactCorpus>() == 2 * std::mem::size_of::<usize>());
};

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_client_index::ContactCorpusRead for NestContactCorpus {
    /// One `list_addressbooks` + one `query_cards` per book, for the whole
    /// result set — the batching the class resolver requires. The alternative,
    /// a read per hit, turns a page of contact rows into a page of round trips.
    async fn read_corpus(&self) -> Result<Vec<fauna_client_index::LocatedContact>, String> {
        // Asked per read rather than held (see the type's docs). `None` means the
        // MSEK went away under us — mail disabled mid-session, or a transient
        // config-load failure. The attach gate in `local_search_index` means this
        // is normally unreachable, and an `Err` is the right answer when it is
        // not: the caller drops the contact rows for this query and the next one
        // asks again (`local_search.rs` — *a failed read is an unresolvable hit*).
        let keys = self
            .keys
            .get()
            .await
            .ok_or_else(|| "mail keys unavailable".to_string())?;
        let carddav = fauna_client_carddav::CardDavClient::new(Arc::clone(&self.nest));
        // Derived once for the whole corpus read — the book listing AND every
        // book's card page below reuse it instead of each paying its own
        // X-Wing keygen .
        let dav_keys =
            fauna_client_carddav::DavRecipientKeys::from_mseks(&keys.msek, &keys.prior_mseks);
        let books = carddav
            .list_addressbooks_decoded(
                fauna_protocol::bridge_routing::ListAddressbooksRequest {
                    actor_id: keys.actor_id.to_vec(),
                },
                &dav_keys,
            )
            .await
            .map_err(|e| format!("list_addressbooks: {e}"))?;
        let mut out = Vec::new();
        for book in &books {
            let page = carddav
                .query_cards_decoded(
                    fauna_protocol::bridge_routing::QueryCardsRequest {
                        actor_id: keys.actor_id.to_vec(),
                        addressbook_id: book.addressbook_id.clone(),
                        since_modseq: None,
                        after_card_id: None,
                        limit: 0,
                    },
                    &dav_keys,
                )
                .await
                .map_err(|e| format!("query_cards: {e}"))?;
            if let fauna_client_carddav::DecodedCardsPage::Ok { cards, .. } = page {
                out.extend(cards.iter().map(|c| {
                    let indexable = indexable_contact(c);
                    fauna_client_index::LocatedContact {
                        uid_hash: indexable.uid_hash,
                        // The snippet renders the card's **current** text — the
                        // honesty rule that makes an edited card's stale index
                        // copy invisible even before the walk catches up.
                        body: if indexable.text.is_empty() {
                            c.parsed.formatted_name.clone()
                        } else {
                            indexable.text
                        },
                    }
                }));
            }
        }
        Ok(out)
    }
}

/// The production [`fauna_client_index::PostCorpusRead`] — the query-time
/// per-hit `fauna.posts.get` that resolves post hits (`content-index.md`
/// § Where queries run: the class resolver in its per-hit shape).
///
/// Holds only the connection: unlike [`NestContactCorpus`] there is no key
/// here to have custody of — a post resolves over the authed wire, and its
/// text comes out of `fauna_core::data::Post::body_text` on the fetched bytes,
/// the same extraction every other reader of a post uses.
#[cfg(not(target_arch = "wasm32"))]
struct NestPostCorpus {
    nest: Arc<NestClient>,
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_client_index::PostCorpusRead for NestPostCorpus {
    async fn read_posts(&self, post_ids: &[String]) -> std::collections::HashMap<String, String> {
        let posts = fauna_client_posts::PostsClient::new(Arc::clone(&self.nest));
        let mut out = std::collections::HashMap::new();
        for id in post_ids {
            // A `not_found` (deleted, or withheld under a legal takedown) and a
            // transport failure land the same way: the id stays out of the map
            // and the hit DROPS — the display-healed deletion, and the
            // no-local-rows offline posture, in one shape.
            let Ok(reply) = posts.posts_get(id.clone()).await else {
                continue;
            };
            let Some(post) = fauna_core::data::Post::decode_resolved_bytes(&reply.body) else {
                continue;
            };
            let text = post.body_text();
            // An empty current text (a body withheld after indexing) has
            // nothing honest to render — same drop as a deletion.
            if !text.is_empty() {
                out.insert(id.clone(), text);
            }
        }
        out
    }
}

/// A stamp identifying the address-book corpus as the wire currently describes
/// it: a digest over every book's `(addressbook_id, ctag)`.
///
/// A digest rather than the raw ctag rows because the only question asked of it
/// is *"is this the corpus I already staged?"* — the ruling reads any difference
/// as "read every book whole", so nothing downstream needs to know which book
/// moved. Order-independent by sorting first, so two devices listing the same
/// books in different orders agree.
///
/// Advisory: a lost, rewound or colliding stamp costs one redundant re-read and
/// republish, never a missed change — the walk re-runs at every attach and
/// signal (`fauna_index::IndexManifest::corpus_markers`).
#[cfg(not(target_arch = "wasm32"))]
fn contacts_corpus_marker(books: &[fauna_client_carddav::DecodedAddressbook]) -> Vec<u8> {
    let mut rows: Vec<(Vec<u8>, i64)> = books
        .iter()
        .map(|b| (b.addressbook_id.clone(), b.ctag))
        .collect();
    rows.sort();
    let mut hasher = blake3::Hasher::new();
    for (id, ctag) in rows {
        hasher.update(&(id.len() as u64).to_le_bytes());
        hasher.update(&id);
        hasher.update(&ctag.to_le_bytes());
    }
    hasher.finalize().as_bytes().to_vec()
}

/// The posts walk's corpus marker: the `(created_at, post_id)` pair of the
/// **newest row the last complete walk observed** — a paging position, where the
/// contacts marker is a corpus digest, because an append-shaped walk needs to
/// know *where covered territory begins*, not whether anything anywhere changed
/// (`content-index.md` § Ingest triggers, v1 → the class template, piece 2: the
/// marker is "a ctag; a paging cursor").
///
/// Advisory like every corpus marker: a lost or unparseable one costs a full
/// re-page that the re-index guard turns into zero republished docs.
#[cfg(not(target_arch = "wasm32"))]
fn posts_corpus_marker(created_at_micros: i64, post_id: &str) -> Vec<u8> {
    format!("{created_at_micros}:{post_id}").into_bytes()
}

/// Decode a posts corpus marker; `None` for a missing or corrupt one, which the
/// walk reads as *unknown* — page everything, never as *unchanged*.
#[cfg(not(target_arch = "wasm32"))]
fn parse_posts_corpus_marker(marker: &[u8]) -> Option<(i64, String)> {
    let s = std::str::from_utf8(marker).ok()?;
    let (at, id) = s.split_once(':')?;
    Some((at.parse().ok()?, id.to_string()))
}

/// The contacts walk's wire seam — `list_addressbooks` + a whole-book read, as
/// a trait so the marker/abandon logic below is provable without a nest.
///
/// This is the shape the posts arm landed first ([`PostsEnumeration`]) and the
/// answer to a gap, which faulted the contacts walk
/// for having its **abandon** branch — the one whose mistake deletes a user's
/// contacts from search — reachable only through a live CardDAV server, and so
/// covered by no test at all. Splitting deciding from doing is what makes every
/// branch reachable from a unit test; the production impl below is the only part
/// that needs a nest, and it holds no logic to get wrong.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
trait ContactsEnumeration: Send + Sync {
    /// Every address book, each carrying the `ctag` the corpus marker digests.
    async fn list_books(&self) -> Result<Vec<fauna_client_carddav::DecodedAddressbook>, String>;
    /// One book, whole — the walk never pages a book, because a prefix of a book
    /// is not a smaller correct corpus (`limit: 0` on the wire).
    async fn read_book(
        &self,
        addressbook_id: Vec<u8>,
    ) -> Result<fauna_client_carddav::DecodedCardsPage, String>;
}

/// What a contacts walk decided, separated from what doing it means so the
/// deciding is unit-testable — [`PostsWalkPlan`]'s twin in the snapshot flavor.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, PartialEq)]
enum ContactsWalkPlan {
    /// Every book's `ctag` is exactly where the marker left it — one RPC,
    /// nothing staged, marker untouched.
    Unchanged,
    /// A read failed mid-walk. Stage nothing, move no marker.
    ///
    /// **This is the arm with teeth, and the snapshot flavor is why**: contacts
    /// stage as a *snapshot*, so a partial corpus does not merely miss rows — it
    /// tombstones the segments holding the cards that failed to read, deleting
    /// them from search until some later walk succeeds. Abandoning leaves the
    /// previous corpus live and the marker unmoved, so the next sweep retries.
    Abandoned,
    /// The whole corpus, plus the marker digesting the book set it was read from.
    ///
    /// `contacts` may be empty, and that is an **event, not a no-op**: a user who
    /// deleted their last address book has an empty corpus, and staging it is
    /// what retires their contact rows from the index (`IndexBuilder`'s
    /// `snapshot_staged` set exists to tell those two apart).
    Stage {
        contacts: Vec<fauna_client_index::IndexableContact>,
        marker: Vec<u8>,
    },
}

/// List the books, precheck the marker, then read **every book whole** and
/// return the result as this kind's snapshot.
///
/// The ctag precheck is load-bearing rather than an optimisation: the snapshot
/// path deliberately keeps no cross-launch no-change guard (a re-index guard
/// would pin each card's first version forever), so without it every launch
/// republishes an identical corpus onto the tombstone-only rail.
#[cfg(not(target_arch = "wasm32"))]
async fn plan_contacts_walk(
    enumeration: &dyn ContactsEnumeration,
    prev_marker: Option<&[u8]>,
) -> ContactsWalkPlan {
    let books = match enumeration.list_books().await {
        Ok(books) => books,
        Err(e) => {
            tracing::debug!(error = %e, "index: contacts walk could not list address books, retrying on the next signal");
            return ContactsWalkPlan::Abandoned;
        }
    };

    let marker = contacts_corpus_marker(&books);
    if prev_marker == Some(marker.as_slice()) {
        return ContactsWalkPlan::Unchanged;
    }

    // ⚠ The corpus is per KIND, not per book: one staged call carries every
    // book's cards, because staging book-by-book would have each call tombstone
    // the previous book's segment (`stage_contact_corpus`).
    let mut contacts = Vec::new();
    for book in &books {
        match enumeration.read_book(book.addressbook_id.clone()).await {
            Ok(fauna_client_carddav::DecodedCardsPage::Ok { cards, .. }) => {
                contacts.extend(cards.iter().map(indexable_contact));
            }
            // The book was deleted between the listing and this read. Its cards
            // are genuinely gone, so an empty contribution is the correct corpus
            // content for it — not a reason to abandon a walk whose other books
            // read fine. (The marker still digests the now-stale book set; the
            // next walk sees a different one and re-reads.)
            Ok(fauna_client_carddav::DecodedCardsPage::AddressbookNotFound) => {}
            // A book we could not read is **not** an empty book — see
            // [`ContactsWalkPlan::Abandoned`].
            Err(e) => {
                tracing::debug!(error = %e, "index: contacts walk could not read a book, leaving the staged corpus untouched");
                return ContactsWalkPlan::Abandoned;
            }
        }
    }
    ContactsWalkPlan::Stage { contacts, marker }
}

/// The production [`ContactsEnumeration`]: CardDAV over the launcher's own
/// connection.
///
/// Holds a [`MailKeys`] for the walk's duration rather than a bare key — the
/// carrier rule (`key-material-hierarchy.md` § Carrier shape), and the same
/// posture [`NestContactCorpus`] uses on the query side. It is a per-walk value,
/// so the clone zeroizes when the walk returns.
#[cfg(not(target_arch = "wasm32"))]
struct NestContactsEnumeration {
    nest: Arc<NestClient>,
    keys: MailKeys,
    /// Derived once at construction (from `keys.msek`) and reused across the
    /// whole walk's `list_books` + every `read_book` call, instead of each
    /// paying its own X-Wing keygen .
    dav_keys: fauna_client_carddav::DavRecipientKeys,
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl ContactsEnumeration for NestContactsEnumeration {
    async fn list_books(&self) -> Result<Vec<fauna_client_carddav::DecodedAddressbook>, String> {
        fauna_client_carddav::CardDavClient::new(Arc::clone(&self.nest))
            .list_addressbooks_decoded(
                fauna_protocol::bridge_routing::ListAddressbooksRequest {
                    actor_id: self.keys.actor_id.to_vec(),
                },
                &self.dav_keys,
            )
            .await
            .map_err(|e| e.to_string())
    }

    async fn read_book(
        &self,
        addressbook_id: Vec<u8>,
    ) -> Result<fauna_client_carddav::DecodedCardsPage, String> {
        fauna_client_carddav::CardDavClient::new(Arc::clone(&self.nest))
            .query_cards_decoded(
                fauna_protocol::bridge_routing::QueryCardsRequest {
                    actor_id: self.keys.actor_id.to_vec(),
                    addressbook_id,
                    since_modseq: None,
                    after_card_id: None,
                    // The wire's unbounded read — the nest takes that branch
                    // explicitly. A page limit here would silently index a
                    // prefix of a real book.
                    limit: 0,
                },
                &self.dav_keys,
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// One page of the self-scoped `fauna.posts.list` enumeration — the posts
/// walk's wire seam, a trait so the paging/marker logic below is provable
/// without a nest (the same coverage the contacts arm was asked
/// for, answered structurally here — and now there too,
/// [`ContactsEnumeration`]).
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
trait PostsEnumeration: Send + Sync {
    async fn page(
        &self,
        cursor_created_at: Option<i64>,
        cursor_post_id: Option<String>,
    ) -> Result<fauna_protocol::posts::PostsListReply, String>;
}

/// What a posts walk decided, separated from what doing it means so the
/// deciding is unit-testable: `run_posts_walk` maps `Stage` onto the
/// `DirectStager` door and the other two onto nothing.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, PartialEq)]
enum PostsWalkPlan {
    /// The corpus is exactly where the marker left it (or is empty) — one RPC,
    /// nothing to stage, marker untouched.
    Unchanged,
    /// A page failed mid-walk. Stage nothing, move no marker — the next sweep
    /// retries from scratch (the contacts abandon rule, append flavor: staging
    /// a prefix would be harmless to existing docs, but advancing the marker
    /// over unpaged territory would hide it from every later walk).
    Abandoned,
    /// Rows newer than the marker, oldest-uncovered to newest, plus the marker
    /// recording the corpus tip this walk observed. `posts` may be empty — a
    /// deleted newest post leaves a marker no row matches, and re-anchoring it
    /// is what stops every later sweep re-paging the whole history.
    Stage {
        posts: Vec<fauna_client_index::IndexablePost>,
        marker: Vec<u8>,
    },
}

/// Page the enumeration newest-first until covered territory (or the end),
/// collecting what the marker says no complete walk has observed.
///
/// A row is **covered** when it orders at-or-below the marker pair on the
/// wire's own `(created_at DESC, post_id DESC)` keyset order — reaching one
/// means every remaining row was observed by the walk that wrote the marker,
/// because a walk only writes its marker after staging everything above its
/// own stop point (induction the `Abandoned` arm preserves).
#[cfg(not(target_arch = "wasm32"))]
async fn plan_posts_walk(
    enumeration: &dyn PostsEnumeration,
    prev_marker: Option<&[u8]>,
) -> PostsWalkPlan {
    let prev = prev_marker.and_then(parse_posts_corpus_marker);
    let covered = |row: &fauna_protocol::posts::PostsListItem| match &prev {
        None => false,
        Some((at, id)) => {
            row.created_at < *at || (row.created_at == *at && row.post_id.as_str() <= id.as_str())
        }
    };

    let mut cursor: (Option<i64>, Option<String>) = (None, None);
    let mut collected = Vec::new();
    let mut new_marker: Option<Vec<u8>> = None;
    loop {
        let reply = match enumeration.page(cursor.0, cursor.1.clone()).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::debug!(error = %e, "index: posts walk could not read a page, leaving the marker unmoved");
                return PostsWalkPlan::Abandoned;
            }
        };
        let marker = match &new_marker {
            Some(marker) => marker.clone(),
            None => {
                // First page. An empty corpus stages nothing and records
                // nothing; a tip identical to the marker is the ctag-precheck
                // equivalent — one RPC and out.
                let Some(first) = reply.posts.first() else {
                    return PostsWalkPlan::Unchanged;
                };
                if prev
                    .as_ref()
                    .is_some_and(|(at, id)| *at == first.created_at && *id == first.post_id)
                {
                    return PostsWalkPlan::Unchanged;
                }
                let marker = posts_corpus_marker(first.created_at, &first.post_id);
                new_marker = Some(marker.clone());
                marker
            }
        };
        for row in &reply.posts {
            if covered(row) {
                return PostsWalkPlan::Stage {
                    posts: collected,
                    marker,
                };
            }
            collected.push(fauna_client_index::IndexablePost {
                post_id: row.post_id.clone(),
                created_at_micros: row.created_at,
                text: row.body.clone(),
            });
        }
        match (reply.cursor_created_at, reply.cursor_post_id) {
            (Some(at), id) => cursor = (Some(at), id),
            (None, _) => {
                return PostsWalkPlan::Stage {
                    posts: collected,
                    marker,
                };
            }
        }
    }
}

/// The production [`PostsEnumeration`]: `fauna.posts.list` over the launcher's
/// own connection. Self-scoped on the wire, so there is no actor to name and
/// no key to hold.
#[cfg(not(target_arch = "wasm32"))]
struct NestPostsEnumeration {
    nest: Arc<NestClient>,
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl PostsEnumeration for NestPostsEnumeration {
    async fn page(
        &self,
        cursor_created_at: Option<i64>,
        cursor_post_id: Option<String>,
    ) -> Result<fauna_protocol::posts::PostsListReply, String> {
        fauna_client_posts::PostsClient::new(Arc::clone(&self.nest))
            .posts_list(fauna_protocol::posts::PostsListRequest {
                cursor_created_at,
                cursor_post_id,
                limit: None,
                extra: Default::default(),
            })
            .await
            .map_err(|e| e.to_string())
    }
}

/// The File walk's wire seam — the cross-set `fauna.media.list` drain plus the
/// one `fauna.folders.list` read that gives each row a *durable* set identity.
///
/// A trait for the reason its two siblings are ([`PostsEnumeration`],
/// [`ContactsEnumeration`]): it makes the paging, the name→id join and the
/// unrenderable-row skip provable without a nest. The production impl below
/// holds no logic to get wrong.
#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
trait FilesEnumeration: Send + Sync {
    /// Every folder this actor can read, for the name→stable-id join.
    ///
    /// Read once per walk rather than per page: the join key is a set-level fact
    /// and a set list is short, so a per-page read would multiply round trips for
    /// an answer that cannot change mid-drain in any way the walk could act on.
    async fn list_sets(&self) -> Result<Vec<fauna_protocol::folders::FolderSummary>, String>;

    /// One keyset page of the cross-set media listing.
    async fn page(
        &self,
        cursor: Option<String>,
    ) -> Result<fauna_protocol::media::MediaListReply, String>;

    /// Render one row's path under the owning set's download keys, or `None`
    /// when this seat cannot open the label.
    ///
    /// Part of the seam rather than the planner because it needs key custody,
    /// which is exactly what a unit test wants to fake — and because the
    /// **skip** it drives is the arm's load-bearing degrade, so a test must be
    /// able to produce it (`content-index.md`: a row whose label this seat
    /// cannot open is skipped without burning the re-index guard).
    async fn render_path(
        &self,
        set_name: &str,
        item: &fauna_protocol::media::MediaItem,
    ) -> Option<String>;
}

/// What a File walk decided, separated from doing it so the deciding is
/// unit-testable — the [`PostsWalkPlan`] shape, one variant lighter.
///
/// **There is no `Unchanged` and no marker**, and both absences are the ruling
/// rather than an unfinished sketch: File keeps no corpus marker in v1, because
/// for an append kind the stage-time guard already makes an unchanged corpus
/// stage nothing and nothing staged publishes nothing. So a walk always drains,
/// and the guard — not a precheck — is what makes the steady state free of rail
/// bytes.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, PartialEq)]
enum FilesWalkPlan {
    /// Nothing to stage: an empty corpus, or a set list that could not be read
    /// (with no set ids, no row can be given a durable identity).
    Nothing,
    /// The rows this drain observed and could render.
    ///
    /// `partial` records that a page read failed mid-drain. Unlike the posts and
    /// contacts walks there is nothing to withhold on it — an append walk's
    /// prefix is *harmless* (every doc is guarded individually and no marker can
    /// be mis-advanced), so the prefix stages and the next walk picks up the
    /// rest. It is carried anyway because it is the difference between "this is
    /// the corpus" and "this is some of it", which the tests assert on and a
    /// future incremental cursor would have to respect.
    Stage {
        files: Vec<fauna_client_index::IndexableFile>,
        partial: bool,
    },
}

/// Drain the cross-set media listing, joining each row to its set's stable id
/// and rendering its sealed path.
///
/// **The join is what makes a rename survivable.** `MediaItem` names its set by
/// *name*, which the user may change at any time; `FolderSummary` carries the
/// durable `id`. Joining per walk means a renamed set's files keep the identity
/// they were indexed under, because the join key is resolved *at this moment* and
/// only its answer — the id — is stamped into the doc.
///
/// The key preferred for that join is the set's `name_hash`, not its plaintext
/// name: post-scrub the plaintext is a sentinel on both planes, while the hash is
/// projected to the same label audience on each and is the nest's own addressing
/// key. The plaintext is the fallback for a set whose name **rests plaintext**:
/// the nest ships `MediaItem::folder_hash` only as a pair with `folder_sealed`
/// (`media_handlers`'s strict-pair rule), so a set no keyed writer has stamped
/// a name seal on carries no hash on its rows even for the label audience —
/// and on that plane the plaintext is unscrubbed, so it still joins. (A
/// non-audience reader never reaches the join: its rows carry no `path_hash`
/// and are skipped below.) Not a compat arm: the compat-remnant sweep classed
/// it LIVE (`docs/goal/architecture/version-compatibility.md` § Dimension 2).
#[cfg(not(target_arch = "wasm32"))]
async fn plan_files_walk(enumeration: &dyn FilesEnumeration) -> FilesWalkPlan {
    let sets = match enumeration.list_sets().await {
        Ok(sets) => sets,
        // No set list, no durable identities — staging under the *name* instead
        // would mint docs a later walk could never match, so the whole walk
        // yields rather than indexing under a key it knows to be wrong.
        Err(e) => {
            tracing::debug!(error = %e, "index: files walk could not list folders, staging nothing");
            return FilesWalkPlan::Nothing;
        }
    };

    // Two lookups over one list. The hash arm is the durable one; the name arm
    // covers a set whose name rests plaintext (no seal stamped, so the nest
    // ships no `folder_hash` on its rows — see the fn doc).
    let mut by_hash: std::collections::HashMap<Vec<u8>, (i64, String)> =
        std::collections::HashMap::new();
    let mut by_name: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for set in &sets {
        // Public-audience folders are public-by-design plaintext and belong
        // to backend 1 (the legacy `web`-mode spelling is retired); reserved `__`
        // sets are structurally excluded. This is an independent guard beside
        // the nest's own filtering — a set that slipped through would
        // otherwise be indexed under the user's own master key.
        if fauna_core::sync::is_reserved_folder_name(&set.name) || set.audience == "public" {
            continue;
        }
        if let Some(hash) = set.name_hash.as_ref() {
            by_hash.insert(hash.to_vec(), (set.id, set.name.clone()));
        }
        if !set.name.is_empty() {
            by_name.insert(set.name.clone(), set.id);
        }
    }

    let mut files = Vec::new();
    let mut cursor: Option<String> = None;
    let mut partial = false;
    loop {
        let reply = match enumeration.page(cursor.clone()).await {
            Ok(reply) => reply,
            Err(e) => {
                tracing::debug!(error = %e, "index: files walk could not read a page, staging the prefix");
                partial = true;
                break;
            }
        };
        for item in &reply.items {
            // Resolve the durable set id. A row whose set is in neither lookup
            // is one this walk cannot give a stable identity — skipped, and left
            // unguarded so a later walk that *can* resolve it stages it.
            let resolved = item
                .folder_hash
                .as_ref()
                .and_then(|h| by_hash.get(h.as_ref()).cloned())
                .or_else(|| {
                    by_name
                        .get(&item.folder)
                        .map(|id| (*id, item.folder.clone()))
                });
            let Some((folder_id, set_name)) = resolved else {
                continue;
            };
            // The identity's other half. A row with no `path_hash` is one this
            // reader is not the label audience for — the same skip, for the
            // same reason. `hex32` rather than a bare
            // `hex::encode` so the 32-byte width is checked at the boundary: a
            // malformed hash becomes this same skip instead of a shorter id that
            // no later walk would ever match.
            let Some(path_hash_hex) = item
                .path_hash
                .as_ref()
                .and_then(|h| <[u8; 32]>::try_from(h.as_ref()).ok())
                .map(|h| fauna_core::hex32::encode(&h))
            else {
                continue;
            };
            // The name, sealed-first. `None` is `SealedLabelRender::Omit`: this
            // seat holds no key for the set (a shared set with no resolver
            // wired), so it has nothing honest to index. Skipped unguarded.
            let Some(path) = enumeration.render_path(&set_name, item).await else {
                continue;
            };
            files.push(fauna_client_index::IndexableFile {
                folder_id,
                path_hash_hex,
                path,
                updated_at_secs: item.updated_at,
            });
        }
        match reply.next_cursor {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }

    if files.is_empty() {
        return FilesWalkPlan::Nothing;
    }
    FilesWalkPlan::Stage { files, partial }
}

/// The production [`FilesEnumeration`]: `fauna.media.list` + `fauna.folders.list`
/// over the launcher's own connection, with the label custody that renders the
/// sealed paths.
///
/// **Custody is built per walk and dropped with it.** The owner root is derived
/// from the identity keypair at construction and the shared-set resolver is
/// whatever glue injected; nothing here outlives the walk, which is the
/// handle-holder posture the contacts arm ratified — ask per use, keep no copy.
///
/// **Every page is judged before anything reads it** (writer-signed change
/// records, ruling (3)): [`FilesEnumeration::page`] keeps only the items the one
/// shared judge admits (`fauna_client_sync::row_judge::judge_media_listing`,
/// under `reader` — this login's actor id and the injected shared-set
/// resolver's nonces). Both consumers page through it — the index walk
/// ([`plan_files_walk`]) and the query-time drain ([`NestFileCorpus`]) — so a
/// row that does not verify is never staged and never resolves a File hit.
#[cfg(not(target_arch = "wasm32"))]
struct NestFilesEnumeration {
    nest: Arc<NestClient>,
    custody: fauna_core::label_custody::LabelCustody,
    reader: fauna_client_sync::row_judge::ReaderSeat,
}

/// The File arm's reader seat: this login's actor id and the set nonces the
/// injected shared-set resolver answers. No resolver (owner-only custody) →
/// no nonces: a signed row cannot verify yet and is held (absent) — and no
/// resolver also means no custody, so the arm yields no rows at all.
#[cfg(not(target_arch = "wasm32"))]
fn file_reader_seat(
    nest: &NestClient,
    resolver: Option<Arc<dyn fauna_core::folder_keys::FolderKeyResolver>>,
    predecessors: Vec<[u8; 32]>,
) -> fauna_client_sync::row_judge::ReaderSeat {
    // `predecessors` is the account's attested predecessor ids, injected beside
    // the resolver (this crate does not reach the account registry): a row a
    // retired identity signed is admitted with no succession lookup (ruling
    // (8)(b) source (ii)). Empty → the seat proves the link itself by the
    // statement walk (`row_judge::LearnedPredecessors`).
    fauna_client_sync::row_judge::ReaderSeat {
        own: fauna_core::hex32::decode(&nest.actor_id_hex()).ok(),
        nonces: resolver.map(fauna_client_sync::SetNonceSource::Resolver),
        predecessors,
        ..Default::default()
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl FilesEnumeration for NestFilesEnumeration {
    async fn list_sets(&self) -> Result<Vec<fauna_protocol::folders::FolderSummary>, String> {
        // `include_shared_with_me` because the ruling includes group-shared sets
        // — the user reads them, which is § What's indexed's "content received
        // from others" posture arriving at the file plane. Without it the join
        // would silently drop every shared set's rows.
        let reply: fauna_protocol::folders::FoldersListReply = self
            .nest
            .request(
                fauna_protocol::folders::KIND_FOLDERS_LIST,
                fauna_protocol::folders::FoldersListRequest {
                    include_shared_with_me: Some(true),
                    extra: Default::default(),
                },
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(reply.folders)
    }

    async fn page(
        &self,
        cursor: Option<String>,
    ) -> Result<fauna_protocol::media::MediaListReply, String> {
        let page = fauna_client_media::MediaClient::new(Arc::clone(&self.nest))
            .list(cursor, fauna_protocol::media::MEDIA_LIST_MAX_LIMIT)
            .await
            .map_err(|e| e.to_string())?;
        fauna_client_sync::row_judge::judge_media_listing(
            &self.nest,
            &self.reader,
            page,
            "content index: fauna.media.list",
        )
        .await
        .map(|(page, _)| page)
        .map_err(|e| e.to_string())
    }

    async fn render_path(
        &self,
        set_name: &str,
        item: &fauna_protocol::media::MediaItem,
    ) -> Option<String> {
        // By the item's own `folder_hash`, never `set_name` alone: the walk
        // resolved `set_name` from the folders list, which is blank for a
        // sealed set once the nest scrubs it (`LabelCustody::keys_for_row`).
        let (keys, _) = self
            .custody
            .keys_for_row(set_name, item.folder_hash.as_ref().map(|b| b.as_ref()))
            .await;
        // Ruling (8)(c): an item whose row was not signed as this account's
        // current identity is staged without the current owner root. This
        // seat holds no predecessor keys, so no owner-family root at all.
        let keys = fauna_core::file_download::FileDownloadKeys {
            record_signer: if item.signed_as_current {
                fauna_core::file_download::RecordSigner::Current
            } else {
                fauna_core::file_download::RecordSigner::Other
            },
            ..keys
        };
        fauna_core::label_custody::render_path(
            &keys,
            item.path_sealed.as_ref().map(|b| b.as_ref()),
            &item.path,
            item.path_hash.as_ref().map(|b| b.as_ref()),
            fauna_core::path_crypto::LabelField::SyncChangePath,
        )
        .text()
        .map(str::to_string)
        .filter(|p| !p.is_empty())
    }
}

/// The production [`fauna_client_index::FileCorpusRead`] — the query-time
/// cross-set drain that resolves File hits.
///
/// Deliberately the **same read the walk makes**, through the same seam: the
/// resolver's three jobs (liveness, current name, `source_online`) all come out
/// of one listing, and sharing the code is what stops the two halves disagreeing
/// about what a file's identity or name is.
#[cfg(not(target_arch = "wasm32"))]
struct NestFileCorpus {
    nest: Arc<NestClient>,
    /// Rebuilt per read for the same reason the walk's is — see
    /// [`NestFilesEnumeration`]. `None` when this login has no identity keypair
    /// to derive an owner root from, which yields *no file rows*.
    resolver: std::sync::Mutex<Option<Arc<dyn fauna_core::folder_keys::FolderKeyResolver>>>,
    /// The account's attested predecessor ids, snapshotted beside `resolver`.
    predecessors: Vec<[u8; 32]>,
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_client_index::FileCorpusRead for NestFileCorpus {
    async fn read_files(&self) -> Result<Vec<fauna_client_index::LocatedFile>, String> {
        let resolver = self.resolver.lock().unwrap().clone();
        let reader = file_reader_seat(&self.nest, resolver.clone(), self.predecessors.clone());
        let Some(custody) = file_label_custody(&self.nest, resolver) else {
            return Ok(Vec::new());
        };
        let enumeration = NestFilesEnumeration {
            nest: Arc::clone(&self.nest),
            custody,
            reader,
        };
        // `source_online` is not on `IndexableFile` (the walk has no use for it),
        // so the resolver re-reads the rows itself rather than reusing the
        // planner's output shape.
        let sets = enumeration.list_sets().await?;
        let mut by_hash: std::collections::HashMap<Vec<u8>, (i64, String)> =
            std::collections::HashMap::new();
        let mut by_name: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for set in &sets {
            if let Some(hash) = set.name_hash.as_ref() {
                by_hash.insert(hash.to_vec(), (set.id, set.name.clone()));
            }
            if !set.name.is_empty() {
                by_name.insert(set.name.clone(), set.id);
            }
        }

        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let reply = enumeration.page(cursor.clone()).await?;
            for item in &reply.items {
                let resolved = item
                    .folder_hash
                    .as_ref()
                    .and_then(|h| by_hash.get(h.as_ref()).cloned())
                    .or_else(|| {
                        by_name
                            .get(&item.folder)
                            .map(|id| (*id, item.folder.clone()))
                    });
                let Some((folder_id, set_name)) = resolved else {
                    continue;
                };
                // Same identity derivation as the walk, deliberately — the two
                // halves must mint the identical spelling or every hit drops.
                let Some(path_hash_hex) = item
                    .path_hash
                    .as_ref()
                    .and_then(|h| <[u8; 32]>::try_from(h.as_ref()).ok())
                    .map(|h| fauna_core::hex32::encode(&h))
                else {
                    continue;
                };
                let Some(path) = enumeration.render_path(&set_name, item).await else {
                    continue;
                };
                out.push(fauna_client_index::LocatedFile {
                    folder_id,
                    path_hash_hex,
                    path,
                    source_online: item.source_online,
                });
            }
            match reply.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }
}

/// The label custody a File walk or resolve renders under, or `None` when this
/// login has no identity keypair.
///
/// The owner root is `BackupKey::derive(identity secret)` — the same derivation
/// every other label-custody site uses — and the shared-set resolver is injected
/// by app glue, because the crate that owns the production resolver
/// (`fauna-client-folders`) depends on *this* one and so cannot be depended on
/// back. A seat with no resolver renders every set it owns and skips the rows of
/// sets shared *with* it; those rows stay unguarded, so the walk on a seat that
/// does hold the keys stages them.
#[cfg(not(target_arch = "wasm32"))]
fn file_label_custody(
    nest: &NestClient,
    resolver: Option<Arc<dyn fauna_core::folder_keys::FolderKeyResolver>>,
) -> Option<fauna_core::label_custody::LabelCustody> {
    let keypair = nest.auth().keypair()?;
    Some(fauna_core::label_custody::LabelCustody::new(
        resolver,
        Some(fauna_core::crypto::BackupKey::derive(
            keypair.secret_bytes(),
        )),
    ))
}

/// One decoded card → the index's view of it.
///
/// The searchable text is the parsed fields rather than the raw vCard, so the
/// index holds what a user would search for (a name, an address, a note) and
/// not the format's own scaffolding — `BEGIN:VCARD`, property names and
/// encoding parameters would otherwise be tokens every card matches on.
#[cfg(not(target_arch = "wasm32"))]
fn indexable_contact(
    card: &fauna_client_carddav::DecodedCard,
) -> fauna_client_index::IndexableContact {
    fauna_client_index::IndexableContact {
        // Hex-lowercase, the spelling the nest's `content_id` conformance pin
        // fixed for every id surface. `uid_hash` is a 32-byte blake3 on the
        // wire; anything else is a row this build cannot address, and a
        // truncated/padded id would silently fail to dedup across devices.
        uid_hash: <[u8; 32]>::try_from(card.uid_hash.as_slice())
            .map(|h| fauna_core::hex32::encode(&h))
            .unwrap_or_default(),
        display_name: Some(card.parsed.formatted_name.clone()).filter(|n| !n.is_empty()),
        text: contact_search_text(&card.parsed),
    }
}

/// The searchable text of one card: every field a user would plausibly recall,
/// joined by newlines.
///
/// The **parsed** fields rather than the raw vCard, so the index holds what a
/// person would search for and not the format's scaffolding — `BEGIN:VCARD`,
/// property names and encoding parameters would otherwise be tokens that every
/// single card matches on, which is worse than useless in a ranked result list.
#[cfg(not(target_arch = "wasm32"))]
fn contact_search_text(card: &fauna_client_carddav::vcard::ParsedVCard) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() {
            parts.push(s);
        }
    };
    push(card.formatted_name.clone());
    if let Some(n) = card.name.as_ref() {
        push(
            [
                n.prefixes.as_str(),
                n.given.as_str(),
                n.additional.as_str(),
                n.family.as_str(),
                n.suffixes.as_str(),
            ]
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(" "),
        );
    }
    for e in &card.emails {
        push(e.value.clone());
    }
    for t in &card.tels {
        push(t.value.clone());
    }
    for u in &card.urls {
        push(u.value.clone());
    }
    for a in &card.addresses {
        push(a.one_line());
    }
    push(card.org_line());
    push(card.title.clone());
    push(card.note.clone());
    parts.join("\n")
}

/// The master-class kinds this seat's builder stages today.
///
/// A hard-coded Rust constant, not a configuration surface — no human chooses it
/// (`principles.md`, the one-configuration-surface invariant's bucket 1). It is
/// the rollout list rather than `ContentKind`'s full master set: a kind belongs
/// here once it has a producer feeding the seam *and* a query-side resolver, so
/// that adding it lights up search rather than building an index the page cannot
/// show (`content-index.md` § Ingest triggers, v1 — *Registration happens at
/// BOTH ends of the pipeline*). Post and contact join as their arms land
/// (rollout S4).
#[cfg(not(target_arch = "wasm32"))]
const MASTER_KINDS_BUILT: &[fauna_client_index::ContentKind] = &[
    fauna_client_index::ContentKind::Conversation,
    fauna_client_index::ContentKind::Draft,
    // Contact's producer is the reconcile walk and its resolver is
    // `ContactCorpusRead`; both are gated on the MSEK rather than on the
    // identity seed the rest of the class rides, so on a seat without mail the
    // walk stages nothing and the reader declines the kind. Listing it here
    // regardless is what lets the arm light up the moment mail arrives, without
    // a second registration path (the attach-later rule).
    fauna_client_index::ContentKind::Contact,
    // Post's producer is its own reconcile walk (append-shaped — the
    // `fauna.posts.list` enumeration) plus the create-time trickle, and its
    // resolver is `PostCorpusRead` (`fauna.posts.get` per hit). Neither needs
    // more than the authed connection every logged-in actor has.
    fauna_client_index::ContentKind::Post,
    // File's producer is the cross-set `fauna.media.list` reconcile walk
    // (append-shaped, no corpus marker — the stage-time guard is the ruled
    // suppression) and its resolver is `FileCorpusRead` (one drain per query).
    // Neither needs the MSEK: a file *name* seals to its set's download keys,
    // whose owner root derives from the identity seed every logged-in actor has.
    // A set shared *with* this actor needs an injected key resolver
    // (`set_folder_key_resolver`); without one its rows are skipped unguarded,
    // so listing the kind unconditionally is again the attach-later rule.
    fauna_client_index::ContentKind::File,
];

#[cfg(not(target_arch = "wasm32"))]
impl NestMailIndexLauncher {
    /// This login's lease gate, or `None` when app glue supplied no seat.
    fn lease_gate(&self) -> Option<Arc<std::sync::atomic::AtomicBool>> {
        self.lease
            .lock()
            .unwrap()
            .as_ref()
            .map(|lease| Arc::clone(&lease.gate))
    }

    /// Hold a fresh arm's first backlog offer until the lease loop has had its
    /// first answer, bounded by `index_lease::FIRST_ANSWER_CEILING` — see the
    /// lease block in [`IndexBuilderLauncher::launch`]. A no-op for an
    /// unseated (uncoordinated) launcher, whose builders have no gate to
    /// consult.
    async fn await_lease_first_answer(&self) {
        let lease = self.lease.lock().unwrap().clone();
        if let Some(lease) = lease
            && !lease.await_first_answer().await
        {
            tracing::warn!(
                "index: the lease's first answer did not land inside {:?}; offering this launch's walks to a gate that errs closed (a stood-down seat re-walks next launch)",
                crate::index_lease::FIRST_ANSWER_CEILING
            );
        }
    }

    /// Resume the **mail** arm and push it into the shared container, unless it
    /// is already there or cannot be built yet. Answers whether it newly
    /// attached.
    ///
    /// Called once from `launch` and again from every mail sweep, so it must be
    /// idempotent and cheap in the steady state: the claim below short-circuits
    /// once the arm exists, and while mail is simply not enabled the key lookup
    /// short-circuits before any rail I/O.
    async fn ensure_mail_arm(&self) -> bool {
        if !self.arms.claim(fauna_client_index::KindClass::MailCal) {
            return false;
        }
        // Mail not provisioned yet — the ordinary state for an actor who has not
        // enabled mail, and the one this whole re-check exists for. `MailKeyCache`
        // re-derives on every call while unset, so the next sweep asks again.
        let Some(keys) = self.keys.get().await else {
            self.arms.release(fauna_client_index::KindClass::MailCal);
            return false;
        };
        // Resume against what this actor has already published: continues the
        // segment chain, inherits the advisory cursor, and — the load-bearing
        // half — seeds the re-index guard, so the mailbox re-walk that is about
        // to start does not republish an index the actor already has.
        match fauna_client_index::resume_mail_builder(
            &keys.msek,
            // Current generation plus a grace key per prior MSEK the custody
            // still holds — the ring the MDA leg builds from the session
            // snapshot, here from the custody itself, so a manifest sealed
            // before a rotation still resumes (and is re-sealed under the
            // current key on the next publish) (`owner-key-material.md`
            // § Path B-sibling-4).
            &keys.index_ring(),
            Arc::clone(&self.publisher) as Arc<dyn fauna_client_index::SegmentRail>,
        )
        .await
        {
            Ok(builder) => {
                let (observer, _builder) = self.spawn_arm(builder, "mail");
                self.arms.push(observer);
                true
            }
            // A blob this build cannot decode is not going to decode on the
            // next sweep either — say so rather than promising recovery. The
            // master arm's twin of this branch; the two stay one shape.
            Err(e) if e.is_permanent() => {
                tracing::warn!(error = %e, "index: the mail/calendar manifest cannot be read by this build — it is intact and untouched, and the sweep retry cannot fix it (update the app)");
                self.arms.release(fauna_client_index::KindClass::MailCal);
                false
            }
            // A transient rail failure (nest unreachable at login) must not cost
            // the user their receive loop. Skipping this arm is recoverable by
            // construction — the next sweep retries it — and blocking the loop
            // would not be. The other arm is still attempted: one class's rail
            // hiccup is not the other's. A wrong-key miss lands here too, and
            // correctly: unlike the master class the mail/calendar key rotates
            // through a grace ring, so the missing generation arrives with the
            // next snapshot sync.
            Err(e) => {
                tracing::warn!(error = %e, "index: could not resume the mail builder, retrying on the next mail sweep");
                self.arms.release(fauna_client_index::KindClass::MailCal);
                false
            }
        }
    }

    /// The master-class twin of [`Self::ensure_mail_arm`] — **one arm for the
    /// whole class**, staging every master kind this seat builds.
    ///
    /// Its precondition — an identity keypair — is present for any logged-in
    /// actor, so unlike mail this normally attaches at launch; the retry path
    /// exists for the transient rail failure, which strands an arm identically.
    ///
    /// One arm rather than one per kind is forced by the manifest: the master
    /// class has a single `manifest.idx`, and a builder rewrites its manifest
    /// wholesale on flush, so two master builders would clobber each other's
    /// segment chains. It also costs nothing here, because every master kind
    /// shares the one precondition this method checks — the identity seed — so
    /// there is no kind that could become buildable at a different moment from
    /// its siblings and want its own attach.
    async fn ensure_master_arm(&self) -> bool {
        if !self.arms.claim(fauna_client_index::KindClass::Master) {
            return false;
        }
        // The key is derived here and never leaves: same posture as the MSEK —
        // app glue passes a `NestClient` and gets back an opaque observer, never
        // key material (`conversations.md` § Architectural rules #2).
        let Some(master_key) = self.master_index_key() else {
            self.arms.release(fauna_client_index::KindClass::Master);
            return false;
        };
        match fauna_client_index::resume_master_builder(
            master_key,
            MASTER_KINDS_BUILT.iter().copied(),
            Arc::clone(&self.publisher) as Arc<dyn fauna_client_index::SegmentRail>,
        )
        .await
        {
            Ok(builder) => {
                let (observer, stager) = self.spawn_arm(builder, "master");
                *self.master_builder.lock().unwrap() = Some(Arc::new(stager));
                self.arms.push(observer);
                true
            }
            // What reaches here is genuinely retryable, and that is now a
            // property rather than a hope: the one permanent cause that used to
            // arrive dressed as a rail hiccup — a manifest sealed under a
            // predecessor's master key, after an identity succession re-derives
            // it from the new seed — is taken by `open_class`'s rebuild arm and
            // never surfaces as an error at all. Retrying that one
            // forever, while a warn promised recovery, left conversation and
            // draft search permanently dead with no witness but a log line.
            Err(e) if e.is_permanent() => {
                // The one thing the rebuild arm deliberately does not cover: a
                // manifest this build cannot decode — typically one a *newer*
                // build wrote. It is intact and must not be touched, so the
                // honest report is "this will not fix itself", never a retry
                // dressed as recovery (`version-compatibility.md` § 5 item 9).
                tracing::warn!(error = %e, "index: the master manifest cannot be read by this build — it is intact and untouched, and the sweep retry cannot fix it (update the app)");
                self.arms.release(fauna_client_index::KindClass::Master);
                false
            }
            Err(e) => {
                tracing::warn!(error = %e, "index: could not resume the master builder, retrying on the next master-kind sweep");
                self.arms.release(fauna_client_index::KindClass::Master);
                false
            }
        }
    }

    /// Attach this login's lease gate to a freshly resumed builder and put it
    /// under the flush debounce — the two steps every arm shares, whether it
    /// resumed at launch or attached a minute later.
    /// Returns the observer to fan out to **and** the staging door behind it —
    /// the latter for the one producer that has no seam to arrive through (the
    /// contacts walk; see [`Self::master_builder`]). A caller with no such
    /// producer drops the handle.
    fn spawn_arm(
        &self,
        builder: fauna_client_index::IndexBuilder,
        arm: &'static str,
    ) -> (
        Arc<dyn MessageIndexObserver>,
        fauna_client_index::DirectStager,
    ) {
        let gate = self.lease_gate();
        let leased = gate.is_some();
        let builder = match gate {
            Some(gate) => builder.with_lease_gate(gate),
            None => builder,
        };
        tracing::info!(
            arm,
            already_indexed = builder.indexed_len(),
            leased,
            "index: builder resumed"
        );
        fauna_client_index::spawn_flush_debounce(Arc::new(builder), self.cancel.clone())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl NestMailIndexLauncher {
    /// The **contacts reconcile walk** — the third ingest class's producer
    /// (`content-index.md` § Ingest triggers, v1 — the contacts ruling).
    ///
    /// `list_addressbooks` (one RPC, carrying each book's `ctag`) → compare the
    /// corpus marker against what was last staged → on any difference read
    /// **every book whole** and stage the result as this kind's snapshot.
    ///
    /// **Idempotent and re-runnable by design**, because it is the correctness
    /// carrier: it runs at arm attach and again on every change signal, and the
    /// ruling puts no correctness weight on the push event at all. Every early
    /// return below is an ordinary state that the next run retries, never an
    /// error to surface — a Search page with no contact rows is the documented
    /// no-reader posture.
    ///
    /// ⚠ **The corpus is per KIND, not per book.** Staging is a single call with
    /// every book's cards in it; staging book-by-book would have each call
    /// tombstone the previous book's segment (`stage_contact_corpus`).
    ///
    /// The deciding lives in [`plan_contacts_walk`] over the [`ContactsEnumeration`]
    /// seam, so every branch — including the abandon — is unit-testable without a
    /// nest; what stays here is the two preconditions that are not enumeration
    /// questions (an attached arm, and an MSEK) plus the staging door.
    async fn run_contacts_walk(&self) {
        // The arm has to exist before there is anywhere to stage. Not yet
        // attached is the ordinary pre-login state; the attach itself runs a
        // walk, so nothing is lost by returning here.
        let Some(stager) = self.master_builder.lock().unwrap().clone() else {
            return;
        };
        // **Contacts are gated on the MSEK, unlike every other master kind.** A
        // card body is sealed to the actor's MSEK-derived recipient keypair, so
        // an actor who has not provisioned mail has no readable address book —
        // and would have no way to resolve a hit even if one were indexed. The
        // cache re-derives while unset, so the next sweep asks again.
        let Some(keys) = self.keys.get().await else {
            return;
        };
        let dav_keys =
            fauna_client_carddav::DavRecipientKeys::from_mseks(&keys.msek, &keys.prior_mseks);
        let enumeration = NestContactsEnumeration {
            nest: Arc::clone(&self.nest),
            keys,
            dav_keys,
        };
        let prev = stager.corpus_marker(fauna_client_index::ContentKind::Contact);
        match plan_contacts_walk(&enumeration, prev.as_deref()).await {
            ContactsWalkPlan::Unchanged | ContactsWalkPlan::Abandoned => {}
            // Stage, stamp and pulse in one call — the marker is noted only after
            // a complete read and rides the same flush as the corpus it
            // describes, and the pulse is what stops that flush waiting out the
            // 60-second backstop (`fauna_client_index::DirectStager`).
            ContactsWalkPlan::Stage { contacts, marker } => {
                if let Err(e) = stager.stage_contact_corpus(&contacts, marker) {
                    tracing::debug!(error = %e, "index: could not record the contacts corpus marker");
                }
            }
        }
    }

    /// The **posts reconcile walk** — the third ingest class's producer, in its
    /// append shape (`content-index.md` § Ingest triggers, v1 — the posts
    /// ruling): page the self-scoped `fauna.posts.list` newest-first, stage
    /// what no complete walk has observed, and re-anchor the corpus marker.
    ///
    /// Same run sites and idempotence contract as the contacts walk — arm
    /// attach and every sweep — and unlike contacts it needs no key at all:
    /// the enumeration rides the authed connection every logged-in actor has.
    /// An unchanged corpus costs one page RPC (the marker precheck inside
    /// [`plan_posts_walk`]); errors abandon with the marker unmoved, so the
    /// next sweep retries.
    /// The **files reconcile walk** — the File arm's only producer
    /// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
    /// SCOPED*): drain the cross-set `fauna.media.list`, join each row to its
    /// set's stable id, render its sealed name, and stage what is not indexed.
    ///
    /// Same run sites and idempotence contract as its two siblings — arm attach,
    /// every sweep, and the `fauna.sync.changed` nudge. Unlike contacts it needs
    /// no MSEK: file names seal to the *set's* download keys, whose owner root
    /// derives from the identity seed every logged-in actor has.
    ///
    /// **No marker precheck, so an unchanged corpus costs a full paged drain and
    /// zero rail bytes** — the ruled suppression for an append kind is the
    /// stage-time guard, not a precheck. The optimization door if that ever hurts
    /// is the actor-keyed `fauna.sync.changes.list` `since` feed, gated on first
    /// verifying its cross-set completeness.
    async fn run_files_walk(&self) {
        let Some(stager) = self.master_builder.lock().unwrap().clone() else {
            return;
        };
        let resolver = self.folder_keys.lock().unwrap().clone();
        let reader = file_reader_seat(
            &self.nest,
            resolver.clone(),
            self.predecessors.lock().unwrap().clone(),
        );
        let Some(custody) = file_label_custody(&self.nest, resolver) else {
            return;
        };
        let enumeration = NestFilesEnumeration {
            nest: Arc::clone(&self.nest),
            custody,
            reader,
        };
        match plan_files_walk(&enumeration).await {
            FilesWalkPlan::Nothing => {}
            // A partial drain stages anyway: appends are guarded per doc and
            // there is no marker to mis-advance, so a prefix is strictly better
            // than nothing and the next walk collects the rest.
            FilesWalkPlan::Stage { files, .. } => stager.stage_files_walk(&files),
        }
    }

    async fn run_posts_walk(&self) {
        let Some(stager) = self.master_builder.lock().unwrap().clone() else {
            return;
        };
        let enumeration = NestPostsEnumeration {
            nest: Arc::clone(&self.nest),
        };
        let marker = stager.corpus_marker(fauna_client_index::ContentKind::Post);
        match plan_posts_walk(&enumeration, marker.as_deref()).await {
            PostsWalkPlan::Unchanged | PostsWalkPlan::Abandoned => {}
            PostsWalkPlan::Stage { posts, marker } => {
                if let Err(e) = stager.stage_posts_walk(&posts, marker) {
                    tracing::debug!(error = %e, "index: could not record the posts corpus marker");
                }
            }
        }
    }

    /// The posts **trickle chokepoint**'s landing spot: stage the caller's own
    /// just-confirmed post, lease-free (`DirectStager::stage_own_post`).
    ///
    /// No arm yet — a phone (`CLIENT_BUILDS_INDEX` false), or a master arm
    /// that has not resumed — drops the call silently: the walk is the
    /// correctness carrier, so the cost is freshness only. The timestamp is
    /// this seat's observation, because `PostCreateReply` carries only the id;
    /// it feeds nothing but the merge's recency tie-break, and is moments from
    /// the nest-assigned `created_at` the walk would stamp.
    pub fn observe_own_post(&self, post_id_hex: &str, body_text: &str) {
        let Some(stager) = self.master_builder.lock().unwrap().clone() else {
            return;
        };
        // Through `Timestamp`, not `SystemTime::now()`: this crate is in the
        // wasm graph, where the std clock panics (`build-system.md` § Wall-clock
        // reads in a crate that can reach wasm). Same epoch-0 fallback and the
        // same i64 saturation the hand-rolled read had.
        let created_at_micros = fauna_core::data::Timestamp::now_or_zero()
            .0
            .min(i64::MAX as u64) as i64;
        stager.stage_own_post(&fauna_client_index::IndexablePost {
            post_id: post_id_hex.to_string(),
            created_at_micros,
            text: body_text.to_string(),
        });
    }

    /// [`Self::observe_own_post`] behind the engine-free seam the create
    /// surfaces speak (`fauna_client_search::OwnPostIndexObserver`) — what app
    /// glue hands `FeedManager::set_post_index_observer`, keeping the never-a-
    /// key posture: glue moves an opaque observer, not a builder.
    pub fn own_post_observer(
        self: &Arc<Self>,
    ) -> Arc<dyn fauna_client_search::OwnPostIndexObserver> {
        struct Adapter(Arc<NestMailIndexLauncher>);
        impl fauna_client_search::OwnPostIndexObserver for Adapter {
            fn own_post_created(&self, post_id_hex: &str, body_text: &str) {
                self.0.observe_own_post(post_id_hex, body_text);
            }
        }
        Arc::new(Adapter(Arc::clone(self)))
    }

    /// This actor's index master key, or `None` before there is an identity to
    /// derive it from.
    ///
    /// `BLAKE3::derive_key("fauna.index.master.v1 2026-08-04", identity_seed)`
    /// — `key-material-hierarchy.md` § Path A-sibling owns the derivation, and
    /// this is its **first production caller**. Deterministic from the seed, so
    /// every one of the user's devices computes the same key with nothing stored
    /// and nothing synced, and two devices logging in concurrently cannot fork
    /// it.
    fn master_index_key(&self) -> Option<fauna_client_index::IndexMasterKey> {
        let keypair = self.nest.auth().keypair()?;
        Some(fauna_client_index::IndexMasterKey::from_bytes(
            *fauna_core::crypto::derive_index_master_key(keypair.secret_bytes()),
        ))
    }
}

/// The native [`LocalIndexResolver`] — lets the Search page mint its local arm
/// whenever the MSEK turns up, instead of only at login.
///
/// [`NestMailIndexLauncher::local_search_index`] answers `None` while mail is
/// unprovisioned, so glue that called it once at login left a user who enabled
/// mail afterwards with a permanently nest-only Search page — the query-side
/// half of the same one-shot gap the build side had, and what kept
/// `test_search_local_index.py` red after the build half was fixed. Registering
/// this instead is synchronous and always possible; the manager resolves on the
/// first query that finds no arm.
///
/// It lives here for the reason the launcher does: this is where the MSEK is,
/// so app glue passes a content lookup and gets back an opaque resolver, never a
/// key. Shared by every native seat (tui + linux directly, windows/apple/android
/// through `fauna-ffi`), so no app carries its own retry.
///
/// **It holds the launcher WEAKLY.** The launcher's flush driver lives exactly
/// as long as the launcher (its `Drop` cancels it), and a search manager is
/// held by page glue whose lifetime this crate cannot see. With a strong hold,
/// any manager a page leaked kept a departed identity's launcher, and so its
/// flush driver, publishing through that identity's `NestClient` once a minute
/// for the life of the process (measured on windows, seven minutes after a
/// succession switch: `transport-connection.md` § No dialer outlives its
/// owner). Once the session's own holders let go, this resolves to `None`,
/// which the page renders as no local rows.
///
/// [`LocalIndexResolver`]: fauna_client_search::LocalIndexResolver
#[cfg(not(target_arch = "wasm32"))]
pub struct LauncherLocalIndex {
    launcher: std::sync::Weak<NestMailIndexLauncher>,
    lookup: Arc<dyn fauna_client_index::MailContentLookup>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LauncherLocalIndex {
    pub fn new(
        launcher: Arc<NestMailIndexLauncher>,
        lookup: Arc<dyn fauna_client_index::MailContentLookup>,
    ) -> Arc<Self> {
        Arc::new(Self {
            launcher: Arc::downgrade(&launcher),
            lookup,
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl fauna_client_search::LocalIndexResolver for LauncherLocalIndex {
    async fn resolve(&self) -> Option<Arc<dyn fauna_client_search::LocalSearchIndex>> {
        let launcher = self.launcher.upgrade()?;
        launcher.local_search_index(Arc::clone(&self.lookup)).await
    }
}

/// Fans one seam callback out to every class's observer — and is the **one
/// object** the manager's single observer slot ever holds for a login.
///
/// Each builder drops what is not its kind (`IndexBuilder::observe_indexable_message`),
/// so fan-out is how one seam feeds N classes without the seam knowing they
/// exist — `fauna_conversations` holds exactly one observer slot by design.
///
/// The arm set is **mutable behind the `Arc`**, because an arm's precondition
/// can arrive mid-session (mail enabled after login). Growing it in place is
/// what lets a late arm join without re-registering: replacing the registered
/// observer would discard the already-running arms' live state — their seeded
/// `(kind, content_id)` re-index guard and anything staged but not yet flushed,
/// neither of which is visible in a search result, so nothing downstream would
/// notice the loss.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct FanOutObserver {
    observers: std::sync::RwLock<Vec<Arc<dyn MessageIndexObserver>>>,
    /// Key **classes** already attached or currently being built — not kinds.
    /// Claimed before the resume's `await` and released if it fails, so a retry
    /// that overlaps an in-flight one cannot produce two builders for a class.
    ///
    /// **Per class rather than per kind, because that is the unit the damage is
    /// measured in:** a class has exactly one manifest, and two builders on one
    /// class would each rewrite it wholesale and silently clobber the other's
    /// segment chain. One builder now spans every kind of its class
    /// (`fauna_client_index::IndexBuilder` holds a kind set), so a second master
    /// kind's sweep must find the master arm already claimed and attach nothing
    /// — which is exactly what keying the claim by class gives.
    claimed: std::sync::Mutex<Vec<fauna_client_index::KindClass>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl FanOutObserver {
    /// Take `class` for building. `false` ⇒ someone already has it.
    fn claim(&self, class: fauna_client_index::KindClass) -> bool {
        let mut claimed = self.claimed.lock().unwrap();
        if claimed.contains(&class) {
            return false;
        }
        claimed.push(class);
        true
    }

    /// Give `class` back after a resume that could not complete, so the next
    /// sweep may try again.
    fn release(&self, class: fauna_client_index::KindClass) {
        self.claimed.lock().unwrap().retain(|c| c != &class);
    }

    fn push(&self, observer: Arc<dyn MessageIndexObserver>) {
        self.observers.write().unwrap().push(observer);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl MessageIndexObserver for FanOutObserver {
    fn observe_indexable_message(&self, msg: IndexableMessage<'_>) {
        for o in self.observers.read().unwrap().iter() {
            o.observe_indexable_message(msg);
        }
    }

    /// Broadcast, exactly like the message callback: the boundary is kind-tagged
    /// and every builder filters for its own, so fanning out to all is what
    /// delivers each arm *its* boundary and nobody else's. Forwarding to only
    /// the first arm would leave the others permanently inside their catch-up
    /// window, so a stood-down seat would gate that class's whole session of
    /// live traffic.
    fn observe_catch_up_complete(&self, kind: IndexableKind) {
        for o in self.observers.read().unwrap().iter() {
            o.observe_catch_up_complete(kind);
        }
    }

    /// Broadcast, the boundary's twin and for its reason: each builder takes
    /// only its own kind, so every arm must hear it.
    fn observe_catch_up_reopened(&self, kind: IndexableKind) {
        for o in self.observers.read().unwrap().iter() {
            o.observe_catch_up_reopened(kind);
        }
    }

    /// Broadcast, for the same reason: the corpus is kind-tagged by
    /// construction, every builder takes only the kinds of its own class, and a
    /// class that does not build drafts drops it in one `contains` check.
    fn observe_draft_corpus(&self, drafts: &[fauna_conversations::index_sink::IndexableDraft]) {
        for o in self.observers.read().unwrap().iter() {
            o.observe_draft_corpus(drafts);
        }
    }
}

// ── Durable inbox-apply backstop (`NestInboxDrainSource`, layer 3) ─────────────

/// The native [`InboxApply`] — routes each durably-fetched inbox item into the
/// conversation engine via the active [`ConversationsSession`]. A Welcome decodes
/// to the same `WelcomeChannelKind` dispatch the receive loop's push arm uses
/// ([`ConversationsSession::ingest_welcome_by_kind`], the SAME free fn — no fork);
/// the as-yet-unwired kinds return **`Err`** so the drain leaves them un-acked
/// (retried, never dropped) per "NEVER `Ok(())` from an apply you didn't perform".
#[cfg(not(target_arch = "wasm32"))]
struct SessionInboxApply {
    session: Arc<ConversationsSession>,
}

#[cfg(not(target_arch = "wasm32"))]
impl InboxApply for SessionInboxApply {
    type Error = String;

    async fn apply_welcome(&self, welcome: WelcomeInbox) -> Result<(), Self::Error> {
        // No channel id → unjoinable (only a peer relay that omits it lands here; the
        // nest carries it on same-nest + cross-nest welcomes). Err →
        // left un-acked (never dropped), so a build that can resolve it applies it.
        let Some(channel_hex) = welcome.channel_id else {
            return Err("welcome inbox item missing channel_id".to_string());
        };
        // The SAME `channel_type`/`group_id` → `WelcomeChannelKind` decode the push
        // arm uses (`NestConversationsPush` → `wire_channel_type_to_kind`), so the
        // drain backstop and the push arm route a scheduling / group / DM welcome
        // identically.
        let kind = wire_channel_type_to_kind(welcome.channel_type, welcome.group_id);
        self.session
            .ingest_welcome_by_kind(
                kind.clone(),
                channel_hex,
                welcome.welcome_bytes,
                // Cross-nest channel home (`nest_url`); blank same-nest.
                welcome.nest_url.unwrap_or_default(),
                FolderWelcomeContext {
                    // The nest-stamped sharer id (folder welcomes only) — the
                    // recipient contact gate reads it to decide auto/knock/suppress;
                    // the drain is the ack authority, so a knock returns `Err` here
                    // and stays un-acked.
                    shared_by: welcome.shared_by,
                    // The home-nest-resolved set name (folder welcomes) — recorded
                    // into the accept-time foreign-set record for a cross-nest share.
                    set_name: welcome.set_name,
                    // The home-nest-resolved access grant, recorded alongside it —
                    // advisory-for-UI, so the client knows whether to offer a bind.
                    access: welcome.access,
                    // The home nest's deployment identity (byte-plane pin trust
                    // root), recorded on the foreign-set record.
                    home_nest_actor_id: welcome.home_nest_actor_id,
                    // The cross-nest owner label, recorded on the foreign-set
                    // record when its own nest verified the pair.
                    shared_by_handle: welcome.shared_by_handle,
                    shared_by_domain: welcome.shared_by_domain,
                    // The sealed set name — what names a cross-nest set once
                    // the join holds its content keys.
                    set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                        welcome.set_name_sealed.as_deref().map(|b| &b[..]),
                        welcome.set_name_hash.as_deref().map(|b| &b[..]),
                    ),
                },
            )
            .await
            // Same door the push arm reports through — the classification (level IS the
            // behaviour) must not be discarded just because this dispatcher
            // flattens to a plain string for the drain's own control flow.
            .inspect_err(|e| fauna_conversations::session::report_welcome_ingest_failure(&kind, e))
            .map_err(|e| e.to_string())
    }

    async fn apply_contact_request(&self, _tuple_bytes: Vec<u8>) -> Result<(), Self::Error> {
        // No native fauna-native contact-request apply path yet (knocks/contacts are
        // read on-demand, not drained). Err → the drain leaves it un-acked (retried,
        // never dropped); wire when the native contacts-apply lands.
        Err("native contact-request apply not yet wired".to_string())
    }

    async fn apply_security_notice(&self, _notice: SecurityNoticeInbox) -> Result<(), Self::Error> {
        // Honest ack: the nest writes the render surface itself — the
        // `notifications` row the Notifications page lists on every app
        // (`notifications.md` § Security notices) — so this inbox envelope is a
        // redundant copy and consuming it loses nothing.
        Ok(())
    }
}

/// Native [`InboxDrainSource`] — the durable inbox-apply **missed-push backstop**
/// (`docs/goal/architecture/api-layers.md` § Inbox & Messaging, layer 3). Held by
/// the [`ConversationsSession`] and driven from its receive-loop ticker; each
/// `drain_once` runs the **shared** orchestration `fauna_client_inbox::drain`
/// (fetch → decode the canonical `InboxEnvelope` → dispatch by `kind` → ack the
/// durably-applied ids; priority #2) over an `InboxClient<Arc<NestClient>>`, with a
/// [`SessionInboxApply`] that ingests Welcomes through the session. The drain twin
/// of [`NestSchedulingSink`].
///
/// Holds a **`Weak`** session to break the session→source→session reference cycle
/// (the session owns this source via `register_inbox_drain`): the receive loop's
/// liveness `Weak` must still fail when the owner drops the session, so the source
/// cannot keep it alive. A dropped session (logout / linux re-injection) makes the
/// next `drain_once` a no-op.
#[cfg(not(target_arch = "wasm32"))]
pub struct NestInboxDrainSource {
    nest: Arc<NestClient>,
    session: Weak<ConversationsSession>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestInboxDrainSource {
    /// `nest` backs the `fauna.inbox.{fetch,ack}` transport; `session` (weak — see
    /// the struct docs) is the apply target.
    pub fn new(nest: Arc<NestClient>, session: Weak<ConversationsSession>) -> Self {
        Self { nest, session }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl InboxDrainSource for NestInboxDrainSource {
    async fn drain_once(&self) -> Result<(), String> {
        // Session dropped (logout / re-injection) — nothing to apply into; a no-op
        // the loop's liveness `Weak` will shortly stop entirely.
        let Some(session) = self.session.upgrade() else {
            return Ok(());
        };
        let client = InboxClient::new(Arc::clone(&self.nest));
        let apply = SessionInboxApply { session };
        let outcome = fauna_client_inbox::drain(&client, &apply, 0)
            .await
            .map_err(|e| e.to_string())?;
        if outcome.applied > 0 || outcome.skipped > 0 {
            tracing::debug!(
                applied = outcome.applied,
                skipped = outcome.skipped,
                more_pending = outcome.more_pending,
                "durable inbox drain pass",
            );
        }
        Ok(())
    }
}

/// The recipient **contact gate** for a cross-user shared folder
/// (`docs/goal/ui/folders.md` § Sharing). Reads the sharer's contact-status over
/// `fauna.contacts.status` (the single-actor lookup —
/// [`ContactsClient::contacts_status`], O(1) on the drain path) and maps it
/// through the shared `fauna_core::data::contact_arrival_disposition`, so **every**
/// client — macOS / iOS / Windows / Android via `fauna-ffi`, linux directly, web
/// through the wasm receive arm — gates an inbound folder Welcome identically
/// with no per-app glue (the contacts-reading twin of [`NestSchedulingSink`]).
/// Natively it is held by the [`ConversationsSession`] via `register_folder_gate`;
/// on web `libs/fauna-wasm`'s `WebInboxApply` passes one straight into the shared
/// `ingest_welcome_by_kind`. Either way the receive rail acts on the returned
/// [`ArrivalDisposition`] (`Auto` → join + ack; `Knock` → stage un-acked;
/// `Suppress` → drop the roster row + ack-and-drop).
///
/// Generic over the transport with two concrete trait shims for exactly the reason
/// [`NestOutboundMailSink`] is (see its doc comment): [`FolderGateSink`] is
/// dual-armed (`Send` futures off wasm, `?Send` on wasm) and `RpcRequester`'s
/// future carries no `Send` bound, so the decision logic lives in the inherent
/// generic [`Self::arrival_for_inner`] and each shim delegates to it.
pub struct NestFolderGate<R: RpcRequester> {
    contacts: ContactsClient<R>,
    /// The same transport, kept raw for the `Suppress`-arm roster drop
    /// ([`Self::drop_roster_row_inner`]) — `fauna.folders.leave` is not a
    /// contacts kind, and this crate cannot reach `FoldersClient` (cycle; see
    /// that method).
    nest: R,
}

impl<R: RpcRequester + Clone> NestFolderGate<R> {
    /// `nest` backs the `fauna.contacts.status` transport (caller-scoped by the
    /// connection — the status is the *recipient's* relationship to the sharer)
    /// and the self-scoped `fauna.folders.leave` the suppress arm issues.
    pub fn new(nest: R) -> Self {
        Self {
            contacts: ContactsClient::new(nest.clone()),
            nest,
        }
    }

    /// Resolve the arrival disposition for a sharer. The single source of the gate
    /// decision both trait shims delegate to — a plain inherent `async fn` so its
    /// future's `Send`-ness is inferred per concrete `R`.
    async fn arrival_for_inner(&self, shared_by: Option<String>) -> ArrivalDisposition {
        // Unstamped (a cross-nest relay that hasn't stamped `shared_by` yet, or a
        // non-folder kind) → `contact_arrival_disposition(None)` = Knock, the safe
        // stranger default: never auto-join without a verified sharer identity.
        let Some(peer_hex) = shared_by else {
            return contact_arrival_disposition(None);
        };
        // Resolve the recipient's contact-status toward the sharer. ANY failure
        // (transport error, or an unrecognized status token) resolves to `None` →
        // Knock — the recipient stays in control; uncertainty never auto-joins.
        let status = match self.contacts.contacts_status(peer_hex).await {
            Ok(reply) => reply.status.as_deref().and_then(ContactStatus::from_wire),
            Err(e) => {
                tracing::debug!("folder gate: contacts.status lookup failed, knocking: {e}");
                None
            }
        };
        contact_arrival_disposition(status)
    }

    /// Drop the recipient's own roster row on a suppressed share — the single
    /// source both trait shims delegate to (same `Send`-inference reason as
    /// [`Self::arrival_for_inner`]).
    ///
    /// Issues the self-scoped `fauna.folders.leave` directly over the transport
    /// rather than through `fauna_client_folders::FoldersClient::leave_with_home`,
    /// which owns this call: `fauna-client-folders/mls` depends on **this** crate
    /// (for `ConversationsClient`'s keypackage/welcome RPCs), so the import would
    /// be a package cycle. The request *shape* is not duplicated across that
    /// boundary, though — both dispatchers build it with
    /// [`fauna_protocol::folders::MemberLeaveRequest::new`], which lists every
    /// field explicitly, so a field added to the leave request is a compile error
    /// in that one constructor rather than a silent default on this path.
    ///
    /// Best-effort per the [`FolderGateSink::drop_roster_row`] contract: any
    /// failure is logged and swallowed, because the caller must still ack a blocked
    /// sharer's Welcome (retaining it would re-surface the arrival the block exists
    /// to hide), and a stale roster row is repaired by the re-share heal.
    async fn drop_roster_row_inner(&self, group_id_hex: String, home_nest_url: Option<String>) {
        let req = fauna_protocol::folders::MemberLeaveRequest::new(group_id_hex, home_nest_url);
        match self
            .nest
            .request::<_, fauna_protocol::folders::MemberLeaveReply>(
                fauna_protocol::folders::KIND_FOLDERS_LEAVE,
                req,
            )
            .await
        {
            Ok(reply) => {
                tracing::debug!(
                    left = reply.left,
                    "folder gate: suppressed share, roster row dropped"
                );
            }
            Err(e) => {
                tracing::warn!(
                    "folder gate: suppressed share, roster drop failed (stale row left for the \
                     re-share heal): {e}"
                );
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait::async_trait]
impl FolderGateSink for NestFolderGate<Arc<NestClient>> {
    async fn arrival_for(&self, shared_by: Option<String>) -> ArrivalDisposition {
        self.arrival_for_inner(shared_by).await
    }

    async fn drop_roster_row(&self, group_id_hex: String, home_nest_url: Option<String>) {
        self.drop_roster_row_inner(group_id_hex, home_nest_url)
            .await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl FolderGateSink for NestFolderGate<fauna_rpc_wasm::WsRpcClient> {
    async fn arrival_for(&self, shared_by: Option<String>) -> ArrivalDisposition {
        self.arrival_for_inner(shared_by).await
    }

    async fn drop_roster_row(&self, group_id_hex: String, home_nest_url: Option<String>) {
        self.drop_roster_row_inner(group_id_hex, home_nest_url)
            .await
    }
}

/// The nest-backed **outbound** dispatcher for the organizer scheduling fork — the
/// SEND counterpart of [`NestSchedulingSink`], shared by every native app (linux
/// directly; macOS / iOS / Windows / Android via the `fauna-ffi` `FfiCaldavClient`).
/// Implements the shared [`ImipDispatch`] seam from `fauna-client-caldav`, wiring
/// its two rails to the two transports this crate (uniquely) reaches: the
/// email-reachable subset over the shared [`EmailClient`] (`fauna.email.send`, the
/// bridge MTA), and each mailbox-less Fauna attendee over the WS-RPC sealed MLS
/// welcome rail via [`ConversationsSession::deliver_scheduling_imip`] (the
/// organizer's loaded MLS engine — Slice 3's `deliver_scheduling_imip`).
///
/// Constructed per dispatch (transient) from the nest handle + the active
/// conversations session and handed to `fauna_client_caldav::dispatch_imip_request`,
/// which owns the *routing* (resolve each recipient, partition email-vs-WS-RPC)
/// while this owns the *rails* (`docs/goal/behavior/caldav-server.md` § Where logic
/// lives — "dispatch routing (email vs WS-RPC) … live in shared Rust"; priority #2,
/// no per-app divergence).
#[cfg(not(target_arch = "wasm32"))]
pub struct NestImipDispatch {
    nest: Arc<NestClient>,
    session: Arc<ConversationsSession>,
}

#[cfg(not(target_arch = "wasm32"))]
impl NestImipDispatch {
    /// `nest` backs the email rail; `session` (the logged-in
    /// [`ConversationsSession`]) backs the mailbox-less MLS rail.
    pub fn new(nest: Arc<NestClient>, session: Arc<ConversationsSession>) -> Self {
        Self { nest, session }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ImipDispatch for NestImipDispatch {
    type Error = String;

    async fn send_email(
        &self,
        recipients: Vec<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), String> {
        EmailClient::new(Arc::clone(&self.nest))
            .send(recipients, raw_rfc5322)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn deliver_mailboxless(
        &self,
        actor_id_hex: &str,
        peer_domain: Option<String>,
        raw_rfc5322: Vec<u8>,
    ) -> Result<(), String> {
        self.session
            .deliver_scheduling_imip(actor_id_hex, peer_domain, raw_rfc5322)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Epoch seconds for the reply-merge re-PUT's CREATED/LAST-MODIFIED surrogate (the
/// crate stays clock-free elsewhere; only the detached calendar merge needs a
/// timestamp).
#[cfg(not(target_arch = "wasm32"))]
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs()
}

/// Map the seam's protocol-agnostic [`WelcomeChannelKind`] onto the typed wire
/// `WelcomeKind` the nest welcome handler expects (variant-for-variant). Shared
/// by the native ([`NestConversationsRpc`]) and wasm ([`WsConversationsRpc`])
/// `welcome_deliver` arms.
fn welcome_kind_to_wire(kind: WelcomeChannelKind) -> WelcomeKind {
    match kind {
        WelcomeChannelKind::Dm => WelcomeKind::Dm,
        WelcomeChannelKind::Group { group_id_hex } => WelcomeKind::Group {
            group_id: group_id_hex,
        },
        WelcomeChannelKind::Scheduling => WelcomeKind::Scheduling,
        WelcomeChannelKind::Folder { group_id_hex } => WelcomeKind::Folder {
            group_id: group_id_hex,
        },
    }
}

/// Inbound-push decode regression guard (the receive twin of the
/// `*_composes_kind_and_payload` wire-contract tests below): a `welcome.received`
/// `PushEvent` maps to a `ConvPushEvent::Welcome` carrying the channel id + bytes,
/// and a `channel.message` maps to the bare poll nudge. A field swap or a kind
/// re-route would otherwise break the receive loop silently. Native-only —
/// `push_event_to_conv` + the push seam don't exist on wasm.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod push_decode_tests {
    use super::push_event_to_conv;
    use fauna_conversations::backend::ConvPushEvent;
    use fauna_protocol::PushEvent;
    use fauna_protocol::push_events::{ChannelMessagePayload, WelcomePayload};

    #[test]
    fn welcome_push_maps_to_welcome_nudge_preserving_channel_and_bytes() {
        let ev = PushEvent::Welcome(WelcomePayload {
            welcome_bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
            channel_id: Some("ab".repeat(32)),
            ..Default::default()
        });
        match push_event_to_conv(ev) {
            Some(ConvPushEvent::Welcome(n)) => {
                assert_eq!(n.channel_id_hex, Some("ab".repeat(32)));
                assert_eq!(n.welcome_bytes, vec![0xDE, 0xAD, 0xBE, 0xEF]);
            }
            other => panic!("expected a Welcome nudge, got {other:?}"),
        }
    }

    #[test]
    fn channel_message_push_maps_to_poll_nudge() {
        let ev = PushEvent::ChannelMessage(ChannelMessagePayload {
            channel_id: "cd".repeat(32),
            data: vec![0x01, 0x02],
            extra: Default::default(),
        });
        assert!(matches!(
            push_event_to_conv(ev),
            Some(ConvPushEvent::ChannelMessage)
        ));
    }
}

// ── Wasm seam impl (the Track E web unblock) ───────────────────────────────

/// Wasm `ConversationsRpc` seam impl — the browser twin of [`NestConversationsRpc`],
/// over `ConversationsClient<WsRpcClient>` (gloo-net `WebSocket` transport). The
/// web SPA registers it on its `FaunaMlsBackend`
/// (`FaunaMlsBackend::new(engine, Arc::new(WsConversationsRpc::new(ws)), …)`),
/// exactly as native apps register `NestConversationsRpc`. Its `!Send`
/// futures satisfy the `#[async_trait(?Send)]` wasm arm of the seam
/// (Track E2a). **Cross-nest** (federated) discovery + relay from the browser is
/// carried too (Spec Y2): `actor_by_handle_remote` opens the anon hop to the peer
/// nest over the wasm `AnonymousWsRpcClient`, and the `peer_domain` relay paths
/// derive the relay `nest_url` via the shared
/// [`fauna_provisioning::probe::peer_nest_url`] — exactly as the
/// native arm does, so the home nest signs + forwards the keypackage fetch /
/// Welcome to the peer nest.
#[cfg(target_arch = "wasm32")]
pub struct WsConversationsRpc {
    client: ConversationsClient<fauna_rpc_wasm::WsRpcClient>,
    /// A raw `WsRpcClient` clone (a cheap `Rc` handle) kept alongside the typed
    /// `ConversationsClient` so the attachment blob put/get can reach
    /// `nest_url()` + `bearer()` for the nest's content-addressed `/api/v1/blob`
    /// HTTP surface — which rides HTTP, not WS-RPC. Mirrors native
    /// `NestConversationsRpc` keeping `nest: Arc<NestClient>` beside its
    /// `ConversationsClient`.
    nest: fauna_rpc_wasm::WsRpcClient,
}

#[cfg(target_arch = "wasm32")]
impl WsConversationsRpc {
    pub fn new(nest: fauna_rpc_wasm::WsRpcClient) -> Self {
        Self {
            client: ConversationsClient::new(nest.clone()),
            nest,
        }
    }
}

// The browser twin of `NestConversationsRpc`'s `LinkPreviewRpc` (render-model.md § D4) — over the
// `Rc`-based `WsRpcClient` requester, satisfying the `?Send` wasm arm of the seam.
#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::LinkPreviewRpc for WsConversationsRpc {
    async fn link_preview_resolve(
        &self,
        url: String,
    ) -> Result<fauna_conversations::backend::LinkPreviewResolution, ConvRpcError> {
        map_link_preview(LinkPreviewClient::new(self.nest.clone()).resolve(url).await)
    }
}

// The wasm arm of the floor-roster report seam — same trait, same client,
// the `?Send` async-trait flavour this target uses.
#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::RoomRosterReporter for WsConversationsRpc {
    async fn report(
        &self,
        report: fauna_conversations::backend::RoomRosterReport,
    ) -> fauna_conversations::backend::RoomRosterReportOutcome {
        report_roster(&self.client, report).await
    }
}

// The wasm arm of the read half.
#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::RoomRosterReader for WsConversationsRpc {
    async fn read_roster(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> fauna_conversations::backend::RoomRosterRead {
        read_roster(&self.client, channel_hex, home_nest_url).await
    }

    async fn read_policy_version(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
        version: u64,
    ) -> fauna_conversations::backend::RoomPolicyVersionRead {
        read_policy_version(&self.client, channel_hex, home_nest_url, version).await
    }
}

// The community class's generation read, browser side — the same shared helper
// as the native arm, so the two targets cannot drift on which kind a
// foreign-homed room uses.
#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::RoomGenerationReader for WsConversationsRpc {
    async fn read_generations(
        &self,
        channel_hex: String,
        home_nest_url: Option<String>,
    ) -> Option<Vec<fauna_conversations::backend::RoomGenerationWrap>> {
        read_generations(&self.client, channel_hex, home_nest_url).await
    }
}

// The community class's birth and keying doors, on the same object for the
// same reason. Wired on wasm like every other room seam even though the SPA
// registers no group-reception key seam (the account store is web's declared
// W3 absence): the refusal a founding hits there names the missing key seam,
// which is the true reason, rather than a missing ceremony that would send
// somebody looking in the wrong place.
#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::RoomCeremonyRpc for WsConversationsRpc {
    async fn room_create(
        &self,
        salt_hex: String,
        policy: Vec<u8>,
        reception_pubkey: Vec<u8>,
    ) -> Result<String, ConvRpcError> {
        create_room(&self.client, salt_hex, policy, reception_pubkey).await
    }

    async fn room_publish_generation(
        &self,
        room_id_hex: String,
        mint: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        publish_room_generation(&self.client, room_id_hex, mint).await
    }
    async fn room_invite(
        &self,
        invite: Vec<u8>,
        invitee_node: String,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        invite_to_room(&self.client, invite, invitee_node, home_nest_url).await
    }

    async fn room_accept_invite(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
        home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        accept_room_invite(&self.client, room_id_hex, reception_pubkey, home_nest_url).await
    }

    async fn room_backfill_generations(
        &self,
        room_id_hex: String,
        target_actor_id_hex: String,
        wraps: Vec<Vec<u8>>,
    ) -> Result<(), ConvRpcError> {
        backfill_room_generations(&self.client, room_id_hex, target_actor_id_hex, wraps).await
    }

    async fn room_pending_invitations(
        &self,
    ) -> Result<Vec<fauna_conversations::backend::PendingRoomInvitation>, ConvRpcError> {
        pending_room_invitations(self.nest.clone()).await
    }

    async fn room_settle_invitation(&self, id: i64) -> Result<(), ConvRpcError> {
        settle_room_invitation(self.nest.clone(), id).await
    }

    async fn room_list_invites(
        &self,
        room_id_hex: String,
    ) -> Result<Vec<fauna_conversations::backend::PendingRoomInvite>, ConvRpcError> {
        list_room_invites(&self.client, room_id_hex).await
    }

    async fn room_revoke_invite(
        &self,
        room_id_hex: String,
        invitee_hex: String,
    ) -> Result<bool, ConvRpcError> {
        revoke_room_invite(&self.client, room_id_hex, invitee_hex).await
    }

    async fn room_remove(
        &self,
        room_id_hex: String,
        principal_hex: String,
    ) -> Result<u32, ConvRpcError> {
        remove_from_room(&self.client, room_id_hex, principal_hex).await
    }

    async fn room_leave(
        &self,
        room_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<u32, ConvRpcError> {
        leave_room(&self.client, room_id_hex, home_nest_url).await
    }

    async fn room_set_policy(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        set_room_policy(&self.client, room_id_hex, policy).await
    }

    async fn room_set_labelers(
        &self,
        room_id_hex: String,
        labelers: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        set_room_labelers(&self.client, room_id_hex, labelers).await
    }

    async fn room_transfer_ownership(
        &self,
        room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        transfer_room_ownership(&self.client, room_id_hex, policy).await
    }

    async fn room_set_reception_key(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
    ) -> Result<fauna_conversations::backend::RoomReceptionKeyBound, ConvRpcError> {
        set_room_reception_key(&self.client, room_id_hex, reception_pubkey).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait::async_trait(?Send)]
impl fauna_conversations::backend::ConversationsRpc for WsConversationsRpc {
    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        self.client
            .channel_send(
                channel_id_hex,
                envelope,
                expect_no_commit_since,
                attachment_refs,
            )
            .await
            .map(|r| r.seq)
            .map_err(channel_send_error)
    }

    async fn channel_send_remote(
        &self,
        channel_id_hex: String,
        home_nest_url: String,
        envelope: Vec<u8>,
        expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        self.client
            .channel_send_remote(
                channel_id_hex,
                home_nest_url,
                envelope,
                expect_no_commit_since,
                attachment_refs,
            )
            .await
            .map(|r| r.seq)
            .map_err(channel_send_error)
    }

    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        limit: i64,
        home_nest_url: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        self.client
            .channel_fetch(channel_id_hex, after, limit, home_nest_url)
            .await
            .map(fetched_records)
            .map_err(conv_rpc_error)
    }

    async fn keypackage_count(&self, actor_id_hex: String) -> Result<u64, ConvRpcError> {
        self.client
            .keypackage_count(actor_id_hex)
            .await
            .map(|r| r.count)
            .map_err(conv_rpc_error)
    }

    /// Degrades **every** failure to `Ok(None)` ("roster unreadable"), which is
    /// the seam's documented fail-safe: a federation hop that cannot answer
    /// for a foreign member, or a transient error all mean the same thing to the caller — do not guess at
    /// membership. The add path's fallback is a clear error to the user, so
    /// there is nothing an `Err` here would let it do better.
    ///
    /// The `Some`-ness pick lives HERE, once (the send-pick precedent,
    /// `direct-messages.md` § step 3b): a foreign-homed channel rides the
    /// distinct relay kind `channel.actors_remote`; same-nest stays on the
    /// plain read.
    async fn channel_actors(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<Option<Vec<String>>, ConvRpcError> {
        Ok(match home_nest_url {
            Some(url) => self
                .client
                .channel_actors_remote(channel_id_hex, url)
                .await
                .ok()
                .map(|r| r.actors),
            None => self
                .client
                .channel_actors(channel_id_hex)
                .await
                .ok()
                .map(|r| r.actors),
        })
    }

    async fn actor_by_handle(
        &self,
        handle: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        self.client
            .actor_by_handle(handle)
            .await
            .map(|opt| {
                opt.map(|r| fauna_conversations::backend::ResolvedHandle {
                    actor_id_hex: r.actor_id,
                    echoed_domain: r.domain,
                    addressable: r.addressable,
                })
            })
            .map_err(conv_rpc_error)
    }

    async fn actor_by_handle_remote(
        &self,
        domain: String,
        localpart: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        // The anon hop needs no home-nest connection — the shared free
        // function, browser arm (`federation.md` § Peer-auth model).
        actor_by_handle_remote(&domain, &localpart).await
    }

    async fn keypackage_fetch(
        &self,
        actor_id_hex: String,
        peer_domain: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        // `Some(domain)` → the home nest relays the fetch to the peer nest
        // (Spec Y2); `None` → same-nest. Identical derivation to the native arm.
        self.client
            .keypackage_fetch(actor_id_hex, peer_nest_url(peer_domain))
            .await
            .map(|r| r.key_package)
            .map_err(conv_rpc_error)
    }

    async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<u64, ConvRpcError> {
        self.client
            .keypackage_upload(packages, last_resort)
            .await
            .map(|r| r.stored)
            .map_err(conv_rpc_error)
    }

    async fn welcome_deliver(
        &self,
        recipient_actor_id_hex: String,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        kind: WelcomeChannelKind,
        peer_domain: Option<String>,
    ) -> Result<(), ConvRpcError> {
        // `Some(domain)` → the home nest relays the Welcome to the peer nest
        // (Spec Y2); `None` → same-nest. Identical derivation to the native arm.
        self.client
            .welcome_deliver(
                recipient_actor_id_hex,
                channel_id_hex,
                welcome_bytes,
                welcome_kind_to_wire(kind),
                peer_nest_url(peer_domain),
            )
            .await
            .map(|_| ())
            .map_err(conv_rpc_error)
    }

    async fn blob_put(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
        bytes: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        // Conversation attachments ride the nest's content-addressed blob
        // surface (`POST /api/v1/blob`) over HTTP, not WS-RPC — the canonical
        // shape is native `NestConversationsRpc::blob_put` (same file). The
        // bytes are already sealed client-side under the channel's
        // `derive_blob_key(epoch_secret)` (in `FaunaMlsBackend::send`), so the
        // nest stores them opaque and never holds an opening key; the sidecar
        // mime stays `application/octet-stream`
        // (`UploadSidecar::conversation_attachment`) — the real mime rides
        // inside the seal, exactly as native does it.
        //
        // WHERE: the room's home nest (`conversation-rooms.md` § The home nest
        // → *Attachment bytes*), exactly as native: same-nest under the SPA's
        // own session bearer; foreign-homed DIRECT to the home nest under a
        // short-lived write token this member's own nest relays
        // (`fauna.conversations.blob.write_token.get`). The token is minted per
        // upload here — the web write byte plane has no bearer cache of its own
        // yet (native's `WriteTokenBearer` is `Send`-bound), and a 10-minute
        // token per attachment is cheap.
        //
        // The multipart body is NOT optional: the nest's strict blob verifier
        // (`bins/fauna-nest/src/blob_routes.rs::upload_blob`) rejects any other
        // Content-Type with 400. This arm posted a raw `Uint8Array` body until
        // 2026-07-31, so every real (non-mock) FaunaMls attachment send from web
        // failed nest-side — the native twin had the identical bug until
        // 2026-07-20. Both now go through one shared multipart helper per target
        // (native `fauna_nest_http`, wasm `fauna_rpc_wasm::post_multipart_blob`)
        // rather than open-coding the shape per caller.
        let sidecar = fauna_media::sidecar::UploadSidecar::conversation_attachment().to_dag_cbor();
        let home = home_nest_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        // Who answers the upload decides whether its words may reach the user
        // (`classify_blob_upload_error`) — captured before `home` is consumed.
        let foreign = home.is_some();
        let result = match home {
            None => {
                fauna_rpc_wasm::post_multipart_blob(
                    &self.nest,
                    fauna_rpc_wasm::BLOB_UPLOAD_PATH,
                    &sidecar,
                    &bytes,
                )
                .await
            }
            Some(home) => {
                let minted = self
                    .client
                    .blob_write_token_get(channel_id_hex, home)
                    .await
                    // Same verdict as the native arm, from the same shared
                    // classifier: a refusal is `Rejected` (no retry, localized
                    // sentence), an unreachable home nest stays `Transient`. This
                    // arm needs no verdict slot — it holds the mint's own error,
                    // so there is nothing to disambiguate. The sealed cid is a
                    // diagnostic and goes to the log, not the user's sentence.
                    .map_err(|e| {
                        tracing::warn!(
                            sealed_cid = %sealed_cid_hex,
                            error = %e,
                            "conversations attachment: write-token mint failed"
                        );
                        conv_rpc_error(e)
                    })?;
                fauna_rpc_wasm::post_multipart_blob_with_bearer(
                    home,
                    &minted.token,
                    fauna_rpc_wasm::BLOB_UPLOAD_PATH,
                    &sidecar,
                    &bytes,
                )
                .await
            }
        };
        // The upload itself (the mint above already returned its own verdict).
        // Its text is the responder's own words, so the same shared classifier as
        // the native arm decides whether the user may read them; the words and the
        // sealed cid ride the log either way.
        result.map_err(|e| {
            tracing::warn!(
                sealed_cid = %sealed_cid_hex,
                error = %e,
                "conversations attachment upload failed"
            );
            classify_blob_upload_error(foreign, &e)
        })?;
        Ok(())
    }

    async fn blob_get(
        &self,
        _channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        // Public content-addressed download (`GET /api/v1/blob/{hash}` — no
        // bearer; mirrors native `NestConversationsRpc::blob_get`) from the nest
        // the bytes rest on: the SPA's own nest for a same-nest channel, the
        // room's home nest — reached direct, under the S6 CORS a cross-nest
        // shared-folder fetch already rides — for a foreign-homed one. A 404 is
        // "blob not present" (`Ok(None)` → the receive path skips that
        // attachment).
        let base = match home_nest_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            None => self.nest.nest_url(),
            Some(home) => home.trim_end_matches('/').to_string(),
        };
        let url = format!("{base}/api/v1/blob/{sealed_cid_hex}");
        let resp = gloo_net::http::Request::get(&url)
            .send()
            .await
            .map_err(|e| ConvRpcError::transient(format!("blob_get GET: {e}")))?;
        if resp.status() == 404 {
            return Ok(None);
        }
        if !resp.ok() {
            return Err(ConvRpcError::transient(format!(
                "blob_get: HTTP {}",
                resp.status()
            )));
        }
        // The fetch API hands a body over whole, so the bound here is a
        // declared `Content-Length` refused before the read and the read's
        // own length refused before a byte is opened; the native twin also
        // cuts an undeclared body off mid-read.
        if let Some(declared) = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.trim().parse::<u64>().ok())
        {
            refuse_blob_body_over_limit(declared)?;
        }
        let body = resp
            .binary()
            .await
            .map_err(|e| ConvRpcError::transient(format!("blob_get read body: {e}")))?;
        refuse_blob_body_over_limit(body.len() as u64)?;
        Ok(Some(body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::{RecordingRequester, block_on};
    use fauna_core::identity::ActorKeypair;

    #[test]
    fn constructor_builds_with_default_nest_client() {
        // Smoke-only — instantiates the wrapper to prove the typed-call
        // surface compiles against the real `NestClient`. The
        // round-trip conformance test lives in
        // `bins/fauna-nest/tests/conformance_conversations_channel.rs`
        // (it exercises the actual router dispatch, which a WS-mocked
        // unit test couldn't usefully add to).
        let kp = ActorKeypair::generate();
        let nest = NestClient::new("ws://127.0.0.1:0/ws".into(), kp);
        let _c = ConversationsClient::new(nest);
    }

    /// A mail-key rotation must not cut the client off from its own local
    /// mail/calendar search: the manifest the builder sealed under the
    /// pre-rotation MSEK still opens through the ring the client derives from
    /// its custody, which now names the old MSEK as a grace generation.
    /// Before this, the client leg built a current-only ring and the builder
    /// never resumed after a rotation ("no key in the ring opens it (1 tried:
    /// current + 0 grace)").
    #[test]
    fn the_index_ring_opens_a_manifest_sealed_before_a_mail_key_rotation() {
        const OLD: [u8; 32] = [21u8; 32];
        const NEW: [u8; 32] = [42u8; 32];
        let old_key = fauna_index::IndexSegmentKey::from_bytes(
            *fauna_mls::wrapped_blob::derive_index_segment_key(&OLD),
        );
        let sealed = fauna_index::IndexManifest::empty(
            fauna_index::KindClass::MailCal,
            fauna_index::TOKENIZER_PIPELINE_VERSION,
        )
        .to_sealed_bytes_mailcal(&old_key)
        .expect("seal under the pre-rotation key");

        let after_rotation = MailKeys::from_custody([7; 32], &NEW, &[OLD]);
        after_rotation
            .index_ring()
            .open_manifest(&sealed)
            .expect("the rotated client opens its pre-rotation manifest");

        let never_rotated = MailKeys::from_custody([7; 32], &NEW, &[]);
        assert!(
            never_rotated.index_ring().open_manifest(&sealed).is_err(),
            "a key the custody does not hold stays shut"
        );
    }

    // ── Wire-contract tests ─────────────────────────────────────────────────
    //
    // The smoke test above instantiates `ConversationsClient` over the real
    // `NestClient` but never issues a call, so it can't catch a wrong kind
    // string or a request that no longer serializes to the shape the nest
    // handler decodes. These tests pin both: each `ConversationsClient` method
    // must send its exact `fauna.conversations.*` / `fauna.actor.*` kind and a
    // payload that round-trips back to the typed request. The `group_*` kinds
    // especially are wasm-bound and web-consumed (Track E pairing/MLS) with no
    // other Rust-layer regression guard — a nest-side kind rename would break
    // them silently the moment a UI lands. The pattern mirrors the
    // `RecordingRequester` in `fauna-client-events` / `-snapshots` / `-sync`
    // (transport-free, so it runs on every target including wasm); real
    // end-to-end round-trip conformance lives in the nest-side conversations
    // conformance suites + `conformance_cross_nest_conversations_client.rs`.

    use fauna_protocol::ByteBuf;

    /// The recording mock never errors, but `ConversationsClient<R>` requires
    /// `R::Error: RpcErrorClass` (for [`ConversationsClient::actor_by_handle`]'s
    /// rejection→`Ok(None)` mapping). This trivial type carries that bound; the
    /// mock always answers `Ok`, so `is_rejection` is never reached.
    #[derive(Debug)]
    struct MockError;

    impl std::fmt::Display for MockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "mock error (never constructed)")
        }
    }

    impl RpcErrorClass for MockError {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    /// A transport error that *did* carry a wire `RpcError` (a server rejection),
    /// so [`conv_rpc_error`] can exercise the `as_rpc_error()` →
    /// `action()`/`localized()` classification path that [`MockError`] (a pure
    /// transport fault) never reaches.
    #[derive(Debug)]
    struct WireErr(fauna_protocol::RpcError);

    impl std::fmt::Display for WireErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "wire rejection: {}", self.0.code)
        }
    }

    impl RpcErrorClass for WireErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&fauna_protocol::RpcError> {
            Some(&self.0)
        }
    }

    // Dim 4 (`version-compatibility.md` § 5 item 3): the conversations seam glue
    // is the *one* place that turns a wire `RpcError` into the rail's
    // protocol-agnostic [`ConvRpcError`], consuming the shared
    // `RpcError::action()` classifier + `localized()` renderer (priority #2 — not
    // re-derived per client). This pins the wire-code → class half; the seam →
    // `BackendError::NeedsUpdate` half is pinned in `fauna-conversations`'
    // `outdated_nest_seam_error_surfaces_as_backend_needs_update`.
    #[test]
    fn conv_rpc_error_classifies_outdated_as_needs_update() {
        use fauna_protocol::RpcError;

        // `fauna.nest.outdated` → the distinct, non-retry NeedsUpdate class, with
        // the localized message (not the raw `details`).
        match conv_rpc_error(WireErr(RpcError::nest_outdated())) {
            ConvRpcError::NeedsUpdate { message } => {
                assert_eq!(message, RpcError::nest_outdated().localized());
                assert!(!message.is_empty());
            }
            other => panic!("expected NeedsUpdate for fauna.nest.outdated, got {other:?}"),
        }

        // A definite refusal that won't change on retry → Rejected.
        match conv_rpc_error(WireErr(RpcError::new(
            "fauna.actor.not_found",
            "error.actor",
        ))) {
            ConvRpcError::Rejected { .. } => {}
            other => panic!("expected Rejected for fauna.actor.not_found, got {other:?}"),
        }

        // A transport fault with no wire `RpcError` → retryable Transient.
        match conv_rpc_error(MockError) {
            ConvRpcError::Transient { .. } => {}
            other => panic!("expected Transient for a bare transport fault, got {other:?}"),
        }
    }

    // The foreign `by_handle` hop's outcome is STRUCTURAL at the seam
    // (`ConversationsRpc::actor_by_handle_remote`'s contract; `federation.md`
    // § Peer-auth model → *Discovery-failure semantics*): the backend must be
    // able to tell "a nest answered: no such actor" from "no nest answered",
    // and before this every request error collapsed into `Transient`'s raw
    // string — which is why an unreachable peer read as "just an email address".
    #[test]
    fn remote_by_handle_outcome_is_structural() {
        use fauna_protocol::RpcError;

        // The nest answered with the actor → `Some`. The reply's `domain` is
        // carried through VERBATIM as `echoed_domain`, empty or not: the seam
        // reports what the peer said, and the dialed domain is not substituted
        // in. (It used to be, which made an untrustworthy field look
        // authoritative to the next reader — `federation.md` § Peer-auth model
        // → *Discovery-failure semantics*, **The dial names the peer**.)
        let found = remote_by_handle_outcome::<MockError>(
            "peer.test".into(),
            Ok(ActorByHandleReply {
                actor_id: "ab".repeat(32),
                handle: "bob".into(),
                domain: String::new(),
                addresses: vec![],
                addressable: true,
                extra: Default::default(),
            }),
        )
        .expect("an answer is Ok")
        .expect("found");
        assert_eq!(found.actor_id_hex, "ab".repeat(32));
        assert_eq!(
            found.echoed_domain, "",
            "an empty echo stays empty — the seam does not speak for the peer"
        );
        assert!(found.addressable);

        // And a peer naming a domain it was never reached at is carried
        // verbatim too, NOT sanitized here: the seam's job is to report the
        // answer structurally, and `FaunaMlsBackend::resolve_foreign` is the
        // one place that decides identity — by the domain it dialed. Asserting
        // the raw echo here is what keeps that decision from quietly migrating
        // into this function, where the dial is only incidentally in scope.
        let lying = remote_by_handle_outcome::<MockError>(
            "attacker.test".into(),
            Ok(ActorByHandleReply {
                actor_id: "cd".repeat(32),
                handle: "bob".into(),
                domain: "trusted.test".into(),
                addresses: vec![],
                addressable: true,
                extra: Default::default(),
            }),
        )
        .expect("an answer is Ok")
        .expect("found");
        assert_eq!(
            lying.echoed_domain, "trusted.test",
            "the echo reaches the caller unchanged, to be ignored there"
        );

        // The nest answered `fauna.actor.not_found` → `Ok(None)`, never an Err.
        let not_found = remote_by_handle_outcome(
            "peer.test".into(),
            Err::<ActorByHandleReply, _>(WireErr(RpcError::new(
                "fauna.actor.not_found",
                "error.actor.not_found",
            ))),
        );
        assert!(matches!(not_found, Ok(None)), "got {not_found:?}");

        // A refusal on the disowning allowlist → `Rejected` (a nest answered
        // and disowned the domain). See `real_wire_codes_decide_discovery_end_to_end`
        // for the codes OUTSIDE that allowlist, which must not read as a
        // disowning.
        let refused = remote_by_handle_outcome(
            "peer.test".into(),
            Err::<ActorByHandleReply, _>(WireErr(RpcError::new(
                "fauna.actor.domain_not_local",
                "error.actor.domain_not_local",
            ))),
        );
        assert!(
            matches!(refused, Err(ConvRpcError::Rejected { .. })),
            "got {refused:?}"
        );

        // A transport fault with no wire error → `Transient`, naming the domain
        // so a log line can tell which peer never answered.
        let dead =
            remote_by_handle_outcome("peer.test".into(), Err::<ActorByHandleReply, _>(MockError));
        match dead {
            Err(ConvRpcError::Transient { message }) => {
                assert!(message.contains("peer.test"), "got {message}");
            }
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    /// **The joined test the classifier gap needed.** Drives REAL wire codes —
    /// the ones `bins/fauna-nest` actually answers `fauna.actor.by_handle`
    /// with — through the entire client-side discovery decision:
    /// `RpcError::action()` (`fauna-protocol`) → [`remote_by_handle_outcome`]
    /// (this crate's seam) → `classify_foreign_non_answer` (the rule
    /// `FaunaMlsBackend::resolve_foreign` runs, in `fauna-conversations`).
    ///
    /// Before the fix, `fauna.protocol.rate_limited` came out of this chain as
    /// "fall through to the email rail" for a **known** Fauna domain — a silent
    /// downgrade to plaintext SMTP of a message MLS would have encrypted. The
    /// gap survived because the suites on either side of the classifier each
    /// tested their own half against a hand-built `ConvRpcError`, so nothing
    /// ever asked *which wire codes* reach which arm. This test is that join.
    ///
    /// `federation.md` § Peer-auth model → *Discovery-failure semantics*.
    #[test]
    fn real_wire_codes_decide_discovery_end_to_end() {
        use fauna_conversations::backend::{DomainEvidence, classify_foreign_non_answer};
        use fauna_protocol::RpcError;

        // One real wire code, the whole way: what does the chain finally decide
        // for a domain the client already knows is Fauna, and for one it has
        // read its own conversations and NOT found?
        let decide = |code: &str, known_fauna_domain: bool| {
            let outcome = remote_by_handle_outcome(
                "peer.test".into(),
                Err::<ActorByHandleReply, _>(WireErr(RpcError::new(code, "error.x"))),
            );
            let err = outcome.expect_err("a refusal never resolves a handle");
            classify_foreign_non_answer(
                &err,
                if known_fauna_domain {
                    DomainEvidence::KnownFauna
                } else {
                    DomainEvidence::AbsentFromLoadedEvidence
                },
            )
        };

        // ── The defect. A peer nest that THROTTLES the anonymous discovery
        // probe has disowned nothing, so for a known Fauna domain the resolve
        // is terminal — the chain must never reach the SMTP rail.
        assert!(
            decide("fauna.protocol.rate_limited", true).terminal,
            "a throttled probe must not downgrade a known Fauna peer to plaintext SMTP",
        );
        // …while first contact with an *unknown* domain still resolves as
        // email, exactly as for a domain with no nest at all (case 2).
        assert!(
            !decide("fauna.protocol.rate_limited", false).terminal,
            "first contact with an unadvertised domain is email by ruling",
        );

        // ── …but "unadvertised" is a verdict this client has to EARN. Before
        // the replica restore lands, the account's conversations are unread and
        // its evidence is empty for a reason that says nothing about the
        // account, so the same non-answer is a failed lookup — the cold-start
        // window that otherwise downgrades a peer of months' standing.
        let unloaded = {
            let outcome = remote_by_handle_outcome(
                "peer.test".into(),
                Err::<ActorByHandleReply, _>(WireErr(RpcError::new(
                    "fauna.protocol.rate_limited",
                    "error.x",
                ))),
            );
            let err = outcome.expect_err("a refusal never resolves a handle");
            classify_foreign_non_answer(&err, DomainEvidence::Unloaded)
        };
        assert!(
            unloaded.terminal,
            "an unread store is not evidence of absence, so it cannot license a downgrade",
        );

        // ── And the same must hold for a code this client has never heard of.
        // `action()`'s default arm is `Rejected` BY DESIGN, so it is the seam's
        // allowlist — not the classifier — that keeps an open error class out
        // of the downgrade decision. Without it, every future refusal code
        // reopens the hole on arrival.
        assert!(
            decide("fauna.actor.some_future_refusal", true).terminal,
            "an unrecognised refusal is a non-answer, not a disowning",
        );

        // ── Case 1 is intact — this is the way a fix here overshoots. A nest
        // that genuinely disowns the handle or the domain must STILL fall
        // through to email, *including* for a known Fauna domain, where a real
        // "no such recipient here" answer must not become a hard error.
        for code in BY_HANDLE_DISOWNING_CODES {
            let known = decide(code, true);
            assert!(
                !known.terminal,
                "{code} from a known Fauna domain must still fall through to email",
            );
            assert!(
                !known.proves_fauna_domain,
                "{code} is a refusal, and a refusal vouches for nothing",
            );
            assert!(
                !decide(code, false).terminal,
                "{code} on first contact is email"
            );
        }

        // ── A version-incompatible peer IS a Fauna nest we cannot talk to:
        // terminal whether or not the domain was known, and it proves Fauna.
        let outdated = decide(RpcError::CODE_NEST_OUTDATED, false);
        assert!(
            outdated.terminal,
            "a Fauna nest we cannot talk to is never email"
        );
        assert!(outdated.proves_fauna_domain);
    }

    /// This crate's reply table for the shared [`RecordingRequester`]:
    /// one arm per kind, each the minimal valid shape its `Reply` decodes.
    fn reply(kind: &'static str) -> Vec<u8> {
        use conversations::*;
        match kind {
            "fauna.conversations.channel.send" => {
                fauna_protocol::encode_canonical(&ChannelSendReply {
                    seq: 1,
                    extra: Default::default(),
                })
            }
            "fauna.conversations.channel.fetch" => {
                fauna_protocol::encode_canonical(&ChannelFetchReply {
                    messages: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.conversations.channel.list_for_actor" => {
                fauna_protocol::encode_canonical(&ChannelListForActorReply {
                    channels: vec![],
                    extra: Default::default(),
                })
            }
            "fauna.conversations.keypackage.upload" => {
                fauna_protocol::encode_canonical(&KeypackageUploadReply {
                    stored: 0,
                    extra: Default::default(),
                })
            }
            "fauna.conversations.keypackage.fetch" => {
                fauna_protocol::encode_canonical(&KeypackageFetchReply {
                    key_package: None,
                    extra: Default::default(),
                })
            }
            "fauna.conversations.keypackage.count" => {
                fauna_protocol::encode_canonical(&KeypackageCountReply {
                    count: 0,
                    extra: Default::default(),
                })
            }
            "fauna.actor.by_handle" => fauna_protocol::encode_canonical(&ActorByHandleReply {
                actor_id: "ab".repeat(32),
                handle: "alice".into(),
                domain: "fauna.test".into(),
                addresses: vec![],
                addressable: true,
                extra: Default::default(),
            }),
            "fauna.conversations.welcome.deliver" => {
                fauna_protocol::encode_canonical(&WelcomeDeliverReply {
                    inbox_id: 1,
                    extra: Default::default(),
                })
            }
            "fauna.conversations.room.invite" | "fauna.conversations.room.invite_remote" => {
                fauna_protocol::encode_canonical(&RoomInviteReply {
                    role: "member".into(),
                    extra: Default::default(),
                })
            }
            // The `NestOutboundMailSink` send path — a clean enqueue (no relay
            // errors). The error-join branch is driven by `RemoteErrRequester`.
            "fauna.email.send" => {
                fauna_protocol::encode_canonical(&fauna_protocol::email::SendEmailReply {
                    extra: Default::default(),
                    local_delivered: 1,
                    remote_queued: 0,
                    remote_errors: vec![],
                })
            }
            "fauna.conversations.room.accept_invite"
            | "fauna.conversations.room.accept_invite_remote" => {
                fauna_protocol::encode_canonical(&conversations::RoomAcceptInviteReply {
                    role: "member".into(),
                    extra: Default::default(),
                })
            }
            "fauna.conversations.room.roster_report"
            | "fauna.conversations.room.roster_report_remote" => {
                fauna_protocol::encode_canonical(&conversations::RoomRosterReportReply {
                    members: 0,
                    superseded_by: None,
                    extra: Default::default(),
                })
            }
            other => panic!("RecordingRequester: unhandled kind {other}"),
        }
        .expect("encode reply")
        .to_vec()
    }

    /// 64-char hex id (`channel_id` / `actor_id` / `group_id` are hex strings
    /// on this surface, not raw bytes).
    fn hex32() -> String {
        "ab".repeat(32)
    }

    fn client() -> (
        std::sync::Arc<RecordingRequester>,
        ConversationsClient<std::sync::Arc<RecordingRequester>>,
    ) {
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let client = ConversationsClient::new(rec.clone());
        (rec, client)
    }

    #[test]
    fn a_roster_report_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.room_roster_report(
            hex32(),
            vec![
                conversations::RoomRosterEntryWire {
                    actor: "11".repeat(32),
                    role: Some("owner".into()),
                    extra: Default::default(),
                },
                conversations::RoomRosterEntryWire {
                    actor: "22".repeat(32),
                    role: Some("member".into()),
                    extra: Default::default(),
                },
            ],
            Some(3),
            Some(8),
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.roster_report");
        let req: conversations::RoomRosterReportRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.room_id, hex32());
        assert_eq!(req.policy_version, Some(3));
        assert_eq!(req.commit_seq, Some(8));
        assert_eq!(req.members.len(), 2);
        assert_eq!(req.members[0].role.as_deref(), Some("owner"));
    }

    /// A foreign-homed ACCEPT rides the **distinct relay kind**, and a
    /// same-nest one does not — the seam's pick, made once for both glue arms
    /// off the invitation's `room_node` (`conversation-rooms.md` § Join rules
    /// and invites → *A cross-nest invitation*, ruling 3). Distinct rather
    /// than additive for the reason `RoomAcceptInviteRemoteRequest` states:
    /// an old own-nest ignoring an additive `nest_url` would answer "no
    /// invitation pending" from a room it does not home, indistinguishable
    /// from a genuine lapse.
    #[test]
    fn a_foreign_homed_accept_rides_the_distinct_relay_kind() {
        let (rec, c) = client();
        let role = block_on(accept_room_invite(
            &c,
            hex32(),
            vec![7u8; 4],
            Some("https://home.example".into()),
        ))
        .expect("the home's ack is forwarded");
        assert_eq!(role, "member");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.accept_invite_remote");
        let req: conversations::RoomAcceptInviteRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.room_id, hex32());
        assert_eq!(req.nest_url, "https://home.example");
        assert_eq!(
            req.reception_pubkey,
            vec![7u8; 4],
            "the wrap target rides the relay"
        );

        let (rec, c) = client();
        block_on(accept_room_invite(&c, hex32(), Vec::new(), None)).expect("same-nest");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.accept_invite");
    }

    /// A foreign-homed INVITE rides the **distinct relay kind**, and a
    /// same-nest one does not — the seam's pick, made once for both glue arms
    /// off the room's recorded home (`conversation-rooms.md` § Join rules and
    /// invites → *A cross-nest invitation*, the foreign-inviter leg). The
    /// invitee's node keeps its meaning on both kinds — a foreign invitee's
    /// domain becomes the base URL the room's home dials, an empty one stays
    /// empty for the home to resolve to the relaying nest.
    #[test]
    fn a_foreign_homed_invite_rides_the_distinct_relay_kind() {
        let (rec, c) = client();
        let role = block_on(invite_to_room(
            &c,
            vec![9u8; 5],
            "third.example".into(),
            Some("https://home.example".into()),
        ))
        .expect("the home's ack is forwarded");
        assert_eq!(role, "member");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.invite_remote");
        let req: conversations::RoomInviteRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.invite, vec![9u8; 5], "the signed act rides verbatim");
        assert_eq!(req.nest_url, "https://home.example");
        assert_eq!(
            req.invitee_node, "https://third.example",
            "a foreign invitee's domain is derived to the base URL the home dials"
        );

        let (rec, c) = client();
        block_on(invite_to_room(
            &c,
            vec![9u8; 5],
            String::new(),
            Some("https://home.example".into()),
        ))
        .expect("relayed, invitee on the inviter's own nest");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.invite_remote");
        let req: conversations::RoomInviteRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert!(
            req.invitee_node.is_empty(),
            "an invitee homed here is left for the room's home to resolve"
        );

        let (rec, c) = client();
        block_on(invite_to_room(&c, vec![9u8; 5], String::new(), None)).expect("same-nest");
        let (kind, _) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.invite");
    }

    /// A foreign-homed report rides the **distinct relay kind** — the seam's
    /// same-nest-vs-relay pick, made here once for both glue arms and mirroring
    /// `read_roster`. Distinct rather than additive for the reason
    /// `RoomRosterReportRemoteRequest` states: an old own-nest ignoring an
    /// additive `nest_url` would bootstrap a stray floor on itself as a clean
    /// success while the real home stayed stale.
    #[test]
    fn a_foreign_homed_report_rides_the_distinct_relay_kind() {
        use fauna_conversations::backend::{RoomRosterEntry, RoomRosterReport};
        use fauna_conversations::room::RoomRole;

        let (rec, c) = client();
        let owner = ActorKeypair::generate();
        let outcome = block_on(report_roster(
            &c,
            RoomRosterReport {
                channel_hex: hex32(),
                members: vec![RoomRosterEntry {
                    actor: owner.actor_id(),
                    role: Some(RoomRole::Owner),
                }],
                policy_version: Some(2),
                commit_seq: Some(17),
                home_nest_url: Some("https://home.example".into()),
            },
        ));
        assert!(outcome.delivered(), "the relay's ack counts as delivered");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.room.roster_report_remote");
        let req: conversations::RoomRosterReportRemoteRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.room_id, hex32());
        assert_eq!(req.nest_url, "https://home.example");
        assert_eq!(req.policy_version, Some(2));
        assert_eq!(
            req.commit_seq,
            Some(17),
            "the relayed report carries the commit's position, or the home cannot order it"
        );
        assert_eq!(req.members[0].actor, owner.actor_id().to_hex());
        assert_eq!(req.members[0].role.as_deref(), Some("owner"));
    }

    /// The projection from the backend's `RoomRosterReport` onto the wire.
    ///
    /// Two things are load-bearing and neither is obvious from the types. A
    /// `None` role must stay **absent**, because absent-vs-`member` is the
    /// whole policy-less/governed distinction the nest stores
    /// (`conversation-rooms.md` § Implementation status today — a policy-less room
    /// carries no roles at all); defaulting it here would silently promote
    /// every policy-less room to governed. And the three-role vocabulary must be
    /// spelled exactly as the nest's CHECK constraint expects, or every report
    /// from a governed room is refused as `invalid_params`.
    #[test]
    fn the_projection_keeps_a_policy_less_rooms_missing_roles_missing() {
        use fauna_conversations::backend::{RoomRosterEntry, RoomRosterReport};
        use fauna_conversations::room::RoomRole;

        let (rec, c) = client();
        let owner = ActorKeypair::generate();
        let policy_less_peer = ActorKeypair::generate();
        block_on(report_roster(
            &c,
            RoomRosterReport {
                channel_hex: hex32(),
                members: vec![
                    RoomRosterEntry {
                        actor: owner.actor_id(),
                        role: Some(RoomRole::Owner),
                    },
                    RoomRosterEntry {
                        actor: policy_less_peer.actor_id(),
                        role: None,
                    },
                ],
                policy_version: None,
                commit_seq: Some(5),
                home_nest_url: None,
            },
        ));
        let (_, payload) = rec.recorded();
        let req: conversations::RoomRosterReportRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.commit_seq,
            Some(5),
            "the same-nest report carries the commit's position too"
        );
        assert_eq!(req.members[0].actor, owner.actor_id().to_hex());
        assert_eq!(req.members[0].role.as_deref(), Some("owner"));
        assert_eq!(
            req.members[1].role, None,
            "a role-less member must stay role-less on the wire — absent is what \
             tells the nest this room is policy-less, not governed"
        );
        assert_eq!(req.policy_version, None);
    }

    /// Every role spells the vocabulary the nest's CHECK constraint accepts.
    /// Exhaustive by construction: `room_role_wire` matches without a wildcard,
    /// so a new variant is a compile error there rather than a silent default
    /// here.
    #[test]
    fn every_room_role_spells_the_nests_vocabulary() {
        use fauna_conversations::room::RoomRole;
        assert_eq!(room_role_wire(RoomRole::Owner), "owner");
        assert_eq!(room_role_wire(RoomRole::Admin), "admin");
        assert_eq!(room_role_wire(RoomRole::Member), "member");
    }

    #[test]
    fn channel_send_composes_kind_and_payload() {
        let (rec, c) = client();
        // A gated commit send threads `expect_no_commit_since` onto the wire.
        block_on(c.channel_send(hex32(), vec![0x01, 0x02, 0x03], Some(7), Vec::new()))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.channel.send");
        let req: conversations::ChannelSendRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.channel_id, hex32());
        assert_eq!(req.envelope, vec![0x01, 0x02, 0x03]);
        assert_eq!(req.expect_no_commit_since, Some(7));
    }

    #[test]
    fn channel_send_ungated_omits_the_precondition() {
        let (rec, c) = client();
        block_on(c.channel_send(hex32(), vec![0x09], None, Vec::new())).expect("infallible mock");
        let (_kind, payload) = rec.recorded();
        let req: conversations::ChannelSendRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.expect_no_commit_since, None);
    }

    /// The conversation kind's blob-reachability floor rides the wire: the
    /// sealed cids a send names land in `attachment_refs` verbatim
    /// (`encryption-at-rest.md` § Per-content-kind conformance → Conversation
    /// messages row), and a send without attachments emits no key at all.
    #[test]
    fn channel_send_threads_attachment_refs_onto_the_wire() {
        let (rec, c) = client();
        let refs = vec!["ab".repeat(32), "cd".repeat(32)];
        block_on(c.channel_send(hex32(), vec![0x01], None, refs.clone())).expect("infallible mock");
        let (_kind, payload) = rec.recorded();
        let req: conversations::ChannelSendRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.attachment_refs, refs);

        let (rec, c) = client();
        block_on(c.channel_send(hex32(), vec![0x02], None, Vec::new())).expect("infallible mock");
        let (_kind, payload) = rec.recorded();
        let map: std::collections::BTreeMap<String, fauna_protocol::Value> =
            fauna_protocol::decode_strict(&payload).expect("decodes as a map");
        assert!(
            !map.contains_key("attachment_refs"),
            "a send with no attachments must not emit the key (an absent field stays absent on the wire)"
        );
    }

    /// The device-owned-epoch gate's wire rejection maps to the typed
    /// [`ConvRpcError::StaleCommit`] rebase signal (not the generic `Rejected`),
    /// with `latest_commit_seq` parsed best-effort from the nest `details`.
    #[test]
    fn stale_commit_wire_error_maps_to_stale_commit_variant() {
        let mut err = fauna_protocol::RpcError::new(
            "fauna.conversations.channel.stale",
            "error.conversations.channel.stale",
        );
        err.details = Some(Box::new(fauna_protocol::Value::String(
            "a commit landed since your seq; latest_commit_seq=42".to_string(),
        )));
        match channel_send_error(WireErr(err)) {
            ConvRpcError::StaleCommit { latest_commit_seq } => {
                assert_eq!(latest_commit_seq, Some(42));
            }
            other => panic!("expected StaleCommit, got {other:?}"),
        }
    }

    /// A non-stale rejection still flows through the generic classifier.
    #[test]
    fn non_stale_rejection_is_not_a_stale_commit() {
        let err = fauna_protocol::RpcError::new("fauna.some.other.code", "error.generic");
        assert!(matches!(
            channel_send_error(WireErr(err)),
            ConvRpcError::Rejected { .. }
        ));
    }

    #[test]
    fn channel_fetch_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.channel_fetch(hex32(), 7, 50, None)).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.channel.fetch");
        let req: conversations::ChannelFetchRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.channel_id, hex32());
        assert_eq!(req.after, 7);
        assert_eq!(req.limit, 50);
        assert_eq!(
            req.nest_url, None,
            "same-nest fetch carries no relay nest_url"
        );
    }

    #[test]
    fn channel_fetch_carries_home_nest_url_for_cross_nest_relay() {
        let (rec, c) = client();
        block_on(c.channel_fetch(hex32(), 0, 100, Some("https://home.test".into())))
            .expect("infallible mock");
        let (_kind, payload) = rec.recorded();
        let req: conversations::ChannelFetchRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.nest_url.as_deref(),
            Some("https://home.test"),
            "the channel's home nest URL rides onto the wire as the relay target"
        );
    }

    #[test]
    fn channel_list_for_actor_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.channel_list_for_actor()).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.channel.list_for_actor");
        let _req: conversations::ChannelListForActorRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
    }

    #[test]
    fn keypackage_upload_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.keypackage_upload(vec![vec![0xAA, 0xBB], vec![0xCC]], true))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.keypackage.upload");
        let req: conversations::KeypackageUploadRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(
            req.packages,
            vec![ByteBuf::from(vec![0xAA, 0xBB]), ByteBuf::from(vec![0xCC])]
        );
        assert!(req.last_resort);
    }

    #[test]
    fn keypackage_fetch_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.keypackage_fetch(hex32(), Some("https://peer.example".into())))
            .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.keypackage.fetch");
        let req: conversations::KeypackageFetchRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.actor_id, hex32());
        assert_eq!(req.nest_url.as_deref(), Some("https://peer.example"));
    }

    #[test]
    fn keypackage_count_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.keypackage_count(hex32())).expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.keypackage.count");
        let req: conversations::KeypackageCountRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.actor_id, hex32());
    }

    #[test]
    fn actor_by_handle_composes_kind_and_payload() {
        let (rec, c) = client();
        let reply = block_on(c.actor_by_handle("alice")).expect("infallible mock");
        // The mock answers `Ok`, so the success arm yields `Some`.
        assert!(reply.is_some());
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.actor.by_handle");
        let req: ActorByHandleRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.handle, "alice");
    }

    #[test]
    fn welcome_deliver_composes_kind_and_payload() {
        let (rec, c) = client();
        block_on(c.welcome_deliver(
            hex32(),
            "cd".repeat(32),
            vec![0xDE, 0xAD],
            WelcomeKind::Group {
                group_id: "ef".repeat(32),
            },
            None,
        ))
        .expect("infallible mock");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.conversations.welcome.deliver");
        let req: conversations::WelcomeDeliverRequest =
            fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.recipient_actor_id, hex32());
        assert_eq!(req.channel_id, "cd".repeat(32));
        assert_eq!(req.welcome_bytes, vec![0xDE, 0xAD]);
        assert_eq!(
            req.kind,
            WelcomeKind::Group {
                group_id: "ef".repeat(32)
            }
        );
        assert_eq!(req.nest_url, None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn folder_welcome_maps_between_wire_tag_and_seam_kind() {
        // Receive side: the "folder" channel_type tag decodes to the Folder seam
        // kind (carrying the group id), NOT the historical `_ => Dm` default — a
        // folder Welcome must never surface as a phantom DM.
        let gid = "ab".repeat(32);
        assert_eq!(
            wire_channel_type_to_kind(Some("folder".to_string()), Some(gid.clone())),
            WelcomeChannelKind::Folder {
                group_id_hex: gid.clone()
            }
        );
        // Send side: the Folder seam kind maps back to the wire `WelcomeKind::Folder`,
        // round-tripping the group id (the tag survives the seam both ways).
        assert_eq!(
            welcome_kind_to_wire(WelcomeChannelKind::Folder {
                group_id_hex: gid.clone()
            }),
            WelcomeKind::Folder { group_id: gid }
        );
    }

    // ── NestOutboundMailSink (the shared send sink) ──────────────────────────
    //
    // Transport-free guards (run on every target via the recording mock), exactly
    // as the `ConversationsClient` wire-contract tests above: the sink must send
    // its composed message under `fauna.email.send` with the recipients +
    // `raw_rfc5322` round-tripping, and must surface a non-empty `remote_errors`
    // list as a joined `Err` (its one piece of non-trivial logic). Drive the
    // shared inherent `submit_inner` — the two concrete `OutboundMailSink` trait
    // shims are 1-line delegates to it, and are only implemented for the concrete
    // `Arc<NestClient>` / `WsRpcClient` transports (not the mock).

    #[test]
    fn outbound_mail_sink_composes_send_kind_and_payload() {
        use fauna_protocol::email::SendEmailRequest;
        let rec = std::sync::Arc::new(RecordingRequester::new(reply));
        let sink = NestOutboundMailSink::new(rec.clone());
        block_on(sink.submit_inner(
            vec!["alice@fauna.test".into(), "bob@peer.test".into()],
            vec![0x52, 0x46, 0x43],
        ))
        .expect("clean enqueue (empty remote_errors) is Ok");
        let (kind, payload) = rec.recorded();
        assert_eq!(kind, "fauna.email.send");
        let req: SendEmailRequest = fauna_protocol::decode_strict(&payload).expect("decodes");
        assert_eq!(req.recipients, vec!["alice@fauna.test", "bob@peer.test"]);
        assert_eq!(req.raw_rfc5322, vec![0x52, 0x46, 0x43]);
    }

    /// A one-off mock answering `fauna.email.send` with a non-empty `remote_errors`
    /// list, to drive the sink's relay-error-join branch.
    struct RemoteErrRequester;

    impl RpcRequester for RemoteErrRequester {
        type Error = MockError;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&fauna_protocol::email::SendEmailReply {
                extra: Default::default(),
                local_delivered: 0,
                remote_queued: 2,
                remote_errors: vec!["mx unreachable".into(), "greylisted".into()],
            })
            .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&bytes).expect("decode reply"))
        }
    }

    #[test]
    fn outbound_mail_sink_surfaces_joined_remote_errors() {
        let sink = NestOutboundMailSink::new(RemoteErrRequester);
        let err = block_on(sink.submit_inner(vec!["bob@peer.test".into()], vec![0x01]))
            .expect_err("a non-empty remote_errors list must surface as Err");
        assert_eq!(err, "mx unreachable; greylisted");
    }

    // ── Recipient contact gate (`folders.md` § Sharing — *Recipient side*) ──
    //
    // `NestFolderGate` is the ONE gate behind every app's inbound folder
    // Welcome, so its decision is a security boundary: get it wrong in the
    // permissive direction and a stranger's shared set joins unbidden. The
    // tri-state's *consumption* (Auto → join + ack, Knock → retain un-acked,
    // Suppress → ack-and-drop) is pinned in `fauna-conversations`'
    // `fauna_mls_backend_tests` with a mock gate; these pin the gate's own
    // contact-status → disposition mapping, including its fail-safe arms, over a
    // transport-free mock (so they run on every target, wasm included — the same
    // build the browser's `WebInboxApply` uses).

    /// Answers `fauna.contacts.status` with a fixed status token (`None` = the
    /// reply carried no status: a stranger), recording the peer id it was asked
    /// about so the nest-stamped `shared_by` can be proven to reach the wire.
    #[derive(Clone)]
    struct StatusRequester {
        status: Option<&'static str>,
        asked_about: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl RpcRequester for StatusRequester {
        type Error = MockError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, "fauna.contacts.status", "the gate issues one kind");
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode request");
            let req: fauna_protocol::contacts::KnockActionRequest =
                fauna_protocol::decode_strict(&bytes).expect("decode request");
            *self.asked_about.lock().unwrap() = Some(req.peer_id);
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::contacts::ContactStatusReply {
                    status: self.status.map(str::to_string),
                    extra: Default::default(),
                })
                .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// Always fails the lookup — the transport-fault arm.
    #[derive(Clone)]
    struct FailingRequester;

    impl RpcRequester for FailingRequester {
        type Error = MockError;

        async fn request<Req, Reply>(
            &self,
            _kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(MockError)
        }
    }

    #[test]
    fn folder_gate_maps_contact_status_to_disposition() {
        let peer = hex32();
        for (wire, expected) in [
            ("confirmed", ArrivalDisposition::Auto),
            ("accepted", ArrivalDisposition::Auto),
            ("pending", ArrivalDisposition::Knock),
            ("blocked", ArrivalDisposition::Suppress),
        ] {
            let asked_about = Arc::new(std::sync::Mutex::new(None));
            let gate = NestFolderGate::new(StatusRequester {
                status: Some(wire),
                asked_about: Arc::clone(&asked_about),
            });
            let got = block_on(gate.arrival_for_inner(Some(peer.clone())));
            assert_eq!(
                got, expected,
                "contact-status {wire:?} must map to {expected:?}"
            );
            assert_eq!(
                asked_about.lock().unwrap().as_deref(),
                Some(peer.as_str()),
                "the nest-stamped sharer id must be the peer the gate looks up",
            );
        }
    }

    #[test]
    fn folder_gate_fails_safe_to_knock_never_auto() {
        let peer = hex32();
        // Uncertainty must NEVER auto-join — the recipient stays in control.
        // 1. No `shared_by` at all (an unstamped / cross-nest-relayed welcome):
        //    no lookup is even possible.
        let gate = NestFolderGate::new(FailingRequester);
        assert_eq!(
            block_on(gate.arrival_for_inner(None)),
            ArrivalDisposition::Knock,
        );
        // 2. The lookup fails (nest unreachable mid-drain).
        assert_eq!(
            block_on(gate.arrival_for_inner(Some(peer.clone()))),
            ArrivalDisposition::Knock,
        );
        // 3. No relationship recorded — the plain stranger.
        let stranger = NestFolderGate::new(StatusRequester {
            status: None,
            asked_about: Arc::new(std::sync::Mutex::new(None)),
        });
        assert_eq!(
            block_on(stranger.arrival_for_inner(Some(peer.clone()))),
            ArrivalDisposition::Knock,
        );
        // 4. A status token this client does not understand (a newer nest
        //    vocabulary) — decodes to `None`, so it knocks rather than guessing.
        let unknown = NestFolderGate::new(StatusRequester {
            status: Some("some_future_status"),
            asked_about: Arc::new(std::sync::Mutex::new(None)),
        });
        assert_eq!(
            block_on(unknown.arrival_for_inner(Some(peer))),
            ArrivalDisposition::Knock,
        );
    }

    // ── The contacts walk's ctag precheck ────────────────────────────────────

    fn book(id: u8, ctag: i64) -> fauna_client_carddav::DecodedAddressbook {
        fauna_client_carddav::DecodedAddressbook {
            addressbook_id: vec![id; 32],
            metadata: fauna_client_carddav::AddressbookMetadata {
                displayname: String::new(),
                description: String::new(),
            },
            ctag,
            highestmodseq: 0,
            card_count: 0,
            created_at: 0,
        }
    }

    /// **The ctag precheck's whole job**, and it is load-bearing rather than an
    /// optimisation: the snapshot path keeps no cross-launch no-change guard, so
    /// without a marker that compares equal across launches every launch would
    /// republish an identical corpus onto the tombstone-only rail.
    #[test]
    fn an_unchanged_address_book_produces_an_unchanged_corpus_marker() {
        let books = vec![book(1, 7), book(2, 3)];
        assert_eq!(
            contacts_corpus_marker(&books),
            contacts_corpus_marker(&books),
            "the same books at the same ctags must stamp the same marker, or the \
             walk re-reads and republishes on every single launch"
        );
    }

    /// Order-independent, so two devices — or two nest listings — that return
    /// the same books in a different order agree rather than each re-reading
    /// the corpus the other just staged.
    #[test]
    fn the_corpus_marker_ignores_book_ordering() {
        assert_eq!(
            contacts_corpus_marker(&[book(1, 7), book(2, 3)]),
            contacts_corpus_marker(&[book(2, 3), book(1, 7)])
        );
    }

    /// Every change the wire can express must move the marker, or the walk
    /// suppresses a read it owed: a bumped ctag (a card was written), a book
    /// added, a book removed.
    #[test]
    fn every_kind_of_address_book_change_moves_the_marker() {
        let base = contacts_corpus_marker(&[book(1, 7), book(2, 3)]);
        assert_ne!(
            base,
            contacts_corpus_marker(&[book(1, 8), book(2, 3)]),
            "a bumped ctag means a card was written to that book"
        );
        assert_ne!(
            base,
            contacts_corpus_marker(&[book(1, 7), book(2, 3), book(3, 1)]),
            "a new address book"
        );
        assert_ne!(
            base,
            contacts_corpus_marker(&[book(1, 7)]),
            "a deleted address book — its cards must leave the corpus"
        );
    }

    /// Distinct books must not collide through the concatenation the digest
    /// feeds on: length-prefixing each id is what stops `(id=[1,2], ctag)` and
    /// `(id=[1], ctag')` from hashing the same bytes.
    #[test]
    fn the_corpus_marker_does_not_confuse_adjacent_fields() {
        let mut a = book(1, 0);
        a.addressbook_id = vec![1, 2];
        let mut b = book(1, 0);
        b.addressbook_id = vec![1];
        assert_ne!(contacts_corpus_marker(&[a]), contacts_corpus_marker(&[b]));
    }

    // ── the contacts walk planner ──────────────────────────────────────────
    //
    // the walk's abandon branch — the one whose
    // mistake *deletes* a user's contacts from search, because contacts stage as
    // a snapshot — was reachable only through a live CardDAV server, so no test
    // drove it and a mutation staging the partial corpus reddened nothing. These
    // drive every branch through the `ContactsEnumeration` seam instead.

    fn card(id: u8, name: &str) -> fauna_client_carddav::DecodedCard {
        let vcard =
            format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{name}\r\nFN:{name}\r\nEND:VCARD\r\n");
        fauna_client_carddav::DecodedCard {
            card_id: vec![id; 32],
            uid_hash: vec![id; 32],
            etag: String::new(),
            modseq: 0,
            internal_date: 0,
            parsed: fauna_client_carddav::vcard::parse_vcard(&vcard),
            vcard,
            has_fauna_ext: false,
        }
    }

    fn page(
        cards: Vec<fauna_client_carddav::DecodedCard>,
    ) -> fauna_client_carddav::DecodedCardsPage {
        fauna_client_carddav::DecodedCardsPage::Ok {
            cards,
            highestmodseq: 0,
            more: false,
        }
    }

    /// Serves a fixed listing and per-book read outcomes, counting both — the
    /// walk's whole wire surface.
    struct Books {
        listing: Result<Vec<fauna_client_carddav::DecodedAddressbook>, String>,
        reads: Vec<(
            Vec<u8>,
            Result<fauna_client_carddav::DecodedCardsPage, String>,
        )>,
        listed: std::sync::atomic::AtomicUsize,
        read: std::sync::atomic::AtomicUsize,
    }

    impl Books {
        fn new(
            listing: Result<Vec<fauna_client_carddav::DecodedAddressbook>, String>,
            reads: Vec<(
                Vec<u8>,
                Result<fauna_client_carddav::DecodedCardsPage, String>,
            )>,
        ) -> Self {
            Self {
                listing,
                reads,
                listed: std::sync::atomic::AtomicUsize::new(0),
                read: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn reads_taken(&self) -> usize {
            self.read.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ContactsEnumeration for Books {
        async fn list_books(
            &self,
        ) -> Result<Vec<fauna_client_carddav::DecodedAddressbook>, String> {
            self.listed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.listing.clone()
        }
        async fn read_book(
            &self,
            addressbook_id: Vec<u8>,
        ) -> Result<fauna_client_carddav::DecodedCardsPage, String> {
            self.read.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.reads
                .iter()
                .find(|(id, _)| *id == addressbook_id)
                .map(|(_, outcome)| outcome.clone())
                .unwrap_or_else(|| Err("the fixture has no such book".into()))
        }
    }

    fn staged_names(plan: &ContactsWalkPlan) -> Vec<String> {
        match plan {
            ContactsWalkPlan::Stage { contacts, .. } => contacts
                .iter()
                .map(|c| c.display_name.clone().unwrap_or_default())
                .collect(),
            other => panic!("expected Stage, got {other:?}"),
        }
    }

    /// A first walk (no marker) reads every book whole and stages one corpus for
    /// the kind — the catch-up door, and the per-KIND staging the ruling requires.
    #[tokio::test]
    async fn a_first_contacts_walk_reads_every_book_and_stages_the_whole_corpus() {
        let books = vec![book(1, 7), book(2, 3)];
        let fixture = Books::new(
            Ok(books.clone()),
            vec![
                (vec![1; 32], Ok(page(vec![card(10, "Ada")]))),
                (
                    vec![2; 32],
                    Ok(page(vec![card(20, "Grace"), card(21, "Alan")])),
                ),
            ],
        );
        let plan = plan_contacts_walk(&fixture, None).await;
        assert_eq!(staged_names(&plan), vec!["Ada", "Grace", "Alan"]);
        let ContactsWalkPlan::Stage { marker, .. } = plan else {
            unreachable!()
        };
        assert_eq!(
            marker,
            contacts_corpus_marker(&books),
            "the marker digests the book set this walk actually read"
        );
        assert_eq!(fixture.reads_taken(), 2, "both books were read");
    }

    /// An unchanged corpus is recognized from the listing alone — one RPC,
    /// nothing staged, marker untouched. Without this precheck every launch
    /// republishes an identical corpus onto the tombstone-only rail, so the
    /// read count is the assertion that matters.
    #[tokio::test]
    async fn an_unchanged_contacts_corpus_costs_one_rpc_and_reads_no_book() {
        let books = vec![book(1, 7), book(2, 3)];
        let marker = contacts_corpus_marker(&books);
        let fixture = Books::new(
            Ok(books),
            vec![(vec![1; 32], Ok(page(vec![card(10, "Ada")])))],
        );
        let plan = plan_contacts_walk(&fixture, Some(&marker)).await;
        assert_eq!(plan, ContactsWalkPlan::Unchanged);
        assert_eq!(
            fixture.reads_taken(),
            0,
            "the precheck must stop at the listing"
        );
    }

    /// **The pin.** A book that fails to read abandons the
    /// WHOLE walk — it does not stage what the other books returned.
    ///
    /// A book we could not read is not an empty book. Because contacts stage as a
    /// snapshot, staging the readable prefix would tombstone the segments holding
    /// the unread book's cards, deleting a user's contacts from search until some
    /// later walk succeeds. `Abandoned` carries no corpus and no marker, so both
    /// halves of the rule — stage nothing, move no marker — are unrepresentable
    /// to get wrong here rather than merely untested.
    #[tokio::test]
    async fn a_book_that_fails_to_read_abandons_the_whole_contacts_walk() {
        let fixture = Books::new(
            Ok(vec![book(1, 7), book(2, 3)]),
            vec![
                (vec![1; 32], Ok(page(vec![card(10, "Ada")]))),
                (vec![2; 32], Err("carddav: connection reset".into())),
            ],
        );
        assert_eq!(
            plan_contacts_walk(&fixture, None).await,
            ContactsWalkPlan::Abandoned,
            "the readable book's single card must NOT be staged as the corpus"
        );
    }

    /// A book deleted between the listing and its read contributes nothing, and
    /// that is **not** an abandon: its cards are genuinely gone, so an empty
    /// contribution is the correct corpus content for it. The distinction from
    /// the test above is the whole reason the wire has a typed
    /// `AddressbookNotFound` instead of an error.
    #[tokio::test]
    async fn a_book_deleted_mid_walk_contributes_nothing_without_abandoning() {
        let fixture = Books::new(
            Ok(vec![book(1, 7), book(2, 3)]),
            vec![
                (vec![1; 32], Ok(page(vec![card(10, "Ada")]))),
                (
                    vec![2; 32],
                    Ok(fauna_client_carddav::DecodedCardsPage::AddressbookNotFound),
                ),
            ],
        );
        let plan = plan_contacts_walk(&fixture, None).await;
        assert_eq!(staged_names(&plan), vec!["Ada"]);
    }

    /// A failed listing abandons before any book is read — the same rule one
    /// level up, and the reason it is not `Unchanged`: an unreadable listing says
    /// nothing about whether the corpus changed.
    #[tokio::test]
    async fn a_failed_listing_abandons_the_contacts_walk() {
        let fixture = Books::new(Err("carddav: not authorized".into()), vec![]);
        assert_eq!(
            plan_contacts_walk(&fixture, None).await,
            ContactsWalkPlan::Abandoned
        );
        assert_eq!(fixture.reads_taken(), 0);
    }

    /// A user who deleted their last address book has an **empty corpus**, and
    /// staging it is an event rather than a no-op: it is what retires their
    /// contact rows from the index. Abandoning here instead would leave deleted
    /// contacts searchable forever.
    #[tokio::test]
    async fn an_empty_book_set_stages_an_empty_corpus() {
        let fixture = Books::new(Ok(vec![]), vec![]);
        let plan = plan_contacts_walk(&fixture, Some(b"a marker from when books existed")).await;
        assert_eq!(staged_names(&plan), Vec::<String>::new());
        let ContactsWalkPlan::Stage { marker, .. } = plan else {
            unreachable!()
        };
        assert_eq!(marker, contacts_corpus_marker(&[]));
    }

    /// **The id-space contract behind `SearchNav::Contact`, pinned at the one
    /// place that mints the spelling.** What the index stores for a card — and
    /// therefore what a search row carries — is the hex of its `uid_hash`, so
    /// decoding it yields exactly the bytes
    /// `CardDavClient::locate_card_by_uid_hash` matches a `DecodedCard` on. It
    /// is emphatically **not** the `card_id` an Address Book opens by.
    ///
    /// Both halves fail *silently* if they ever drift — a lookup that matches
    /// nothing raises no error and opens no card — so agreement between the
    /// encode here and the decode in the locate is pinned rather than assumed.
    #[test]
    fn the_indexed_contact_id_round_trips_to_the_uid_hash_the_locate_matches_on() {
        let mut c = card(0xC1, "Ada");
        // The fixture gives a card the same bytes for both ids; a real card's
        // differ, and telling them apart is the whole point here.
        c.card_id = vec![0xAA; 32];
        c.uid_hash = vec![0xBB; 32];

        let indexed = indexable_contact(&c);
        assert_eq!(
            fauna_core::hex32::decode(&indexed.uid_hash)
                .expect("the indexed id is 32-byte hex")
                .to_vec(),
            c.uid_hash,
            "the id the index stores must decode back to the card's own uid_hash"
        );
        assert_ne!(
            indexed.uid_hash,
            fauna_core::hex32::encode(&[0xAA; 32]),
            "never the card_id — that is the OTHER id space, and the confusion \
             this whole join exists to prevent"
        );
        assert_eq!(
            indexed.uid_hash,
            indexed.uid_hash.to_lowercase(),
            "hex-lowercase, the spelling every 32-byte id surface fixed"
        );
    }

    // ── the posts walk planner ─────────────────────────────────────────────

    use fauna_protocol::posts::{PostsListItem, PostsListReply};

    fn row(post_id: &str, created_at: i64, body: &str) -> PostsListItem {
        PostsListItem {
            post_id: post_id.to_string(),
            created_at,
            body: body.to_string(),
            extra: Default::default(),
        }
    }

    /// Serves a fixed page sequence and counts requests — the walk's whole wire
    /// surface, which is what makes every branch below provable without a nest
    /// (the coverage shape a prior finding asked for).
    struct Pages {
        pages: Vec<Result<PostsListReply, String>>,
        served: std::sync::atomic::AtomicUsize,
    }

    impl Pages {
        fn new(pages: Vec<Result<PostsListReply, String>>) -> Self {
            Self {
                pages,
                served: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn of(rows_per_page: Vec<Vec<PostsListItem>>) -> Self {
            let last = rows_per_page.len().saturating_sub(1);
            Self::new(
                rows_per_page
                    .into_iter()
                    .enumerate()
                    .map(|(i, posts)| {
                        let cursor = (i < last)
                            .then(|| posts.last().map(|r| (r.created_at, r.post_id.clone())))
                            .flatten();
                        Ok(PostsListReply {
                            posts,
                            cursor_created_at: cursor.as_ref().map(|(at, _)| *at),
                            cursor_post_id: cursor.map(|(_, id)| id),
                            extra: Default::default(),
                        })
                    })
                    .collect(),
            )
        }
        fn served(&self) -> usize {
            self.served.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PostsEnumeration for Pages {
        async fn page(
            &self,
            _cursor_created_at: Option<i64>,
            _cursor_post_id: Option<String>,
        ) -> Result<PostsListReply, String> {
            let i = self
                .served
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.pages
                .get(i)
                .cloned()
                .unwrap_or_else(|| Err("walked past the last page".into()))
        }
    }

    fn staged_ids(plan: &PostsWalkPlan) -> Vec<String> {
        match plan {
            PostsWalkPlan::Stage { posts, .. } => posts.iter().map(|p| p.post_id.clone()).collect(),
            other => panic!("expected Stage, got {other:?}"),
        }
    }

    /// A first walk (no marker) pages the entire history and stages every row —
    /// the catch-up door the enumeration kind exists to open.
    #[tokio::test]
    async fn a_first_posts_walk_pages_to_the_end_and_stages_everything() {
        let pages = Pages::of(vec![
            vec![row("dd", 4_000, "newest"), row("cc", 3_000, "third")],
            vec![row("bb", 2_000, "second"), row("aa", 1_000, "oldest")],
        ]);
        let plan = plan_posts_walk(&pages, None).await;
        assert_eq!(staged_ids(&plan), vec!["dd", "cc", "bb", "aa"]);
        let PostsWalkPlan::Stage { marker, .. } = plan else {
            unreachable!()
        };
        assert_eq!(
            parse_posts_corpus_marker(&marker),
            Some((4_000, "dd".to_string())),
            "the marker records the corpus tip this walk observed"
        );
        assert_eq!(pages.served(), 2, "both pages were read");
    }

    /// An unchanged corpus is recognized from the first page's tip — one RPC,
    /// nothing staged, marker untouched. This is the precheck that keeps the
    /// per-sweep cadence affordable; without it every sweep re-pages the whole
    /// history (the guard would still publish nothing, so the only witness is
    /// this call count).
    #[tokio::test]
    async fn an_unchanged_corpus_costs_one_page_and_stages_nothing() {
        let pages = Pages::of(vec![
            vec![row("dd", 4_000, "newest"), row("cc", 3_000, "third")],
            vec![row("bb", 2_000, "second")],
        ]);
        let marker = posts_corpus_marker(4_000, "dd");
        let plan = plan_posts_walk(&pages, Some(&marker)).await;
        assert_eq!(plan, PostsWalkPlan::Unchanged);
        assert_eq!(pages.served(), 1, "the precheck must stop at page one");
    }

    /// A walk with a mid-corpus marker stages only the rows above it and stops
    /// inside the first page that reaches covered territory — never re-paging
    /// what the previous complete walk already observed.
    #[tokio::test]
    async fn a_walk_stops_at_the_marker_and_stages_only_the_newer_rows() {
        let pages = Pages::of(vec![
            vec![row("ee", 5_000, "brand new"), row("dd", 4_000, "also new")],
            vec![row("cc", 3_000, "the old tip"), row("bb", 2_000, "older")],
            vec![row("aa", 1_000, "oldest")],
        ]);
        let marker = posts_corpus_marker(3_000, "cc");
        let plan = plan_posts_walk(&pages, Some(&marker)).await;
        assert_eq!(staged_ids(&plan), vec!["ee", "dd"]);
        assert_eq!(
            pages.served(),
            2,
            "the walk stops inside the page that reaches the marker"
        );
    }

    /// Two rows sharing the boundary timestamp: only the one ordering strictly
    /// above the marker on the `(created_at, post_id)` keyset is uncovered —
    /// the tiebreak half doing for the walk what it does for the wire's own
    /// paging (`PostsListRequest::cursor_post_id`).
    #[tokio::test]
    async fn a_tie_on_the_boundary_timestamp_is_split_by_the_id_half() {
        let pages = Pages::of(vec![vec![
            row("cc", 2_000, "tie, newer id"),
            row("bb", 2_000, "the marker row"),
            row("aa", 1_000, "older"),
        ]]);
        let marker = posts_corpus_marker(2_000, "bb");
        let plan = plan_posts_walk(&pages, Some(&marker)).await;
        assert_eq!(
            staged_ids(&plan),
            vec!["cc"],
            "same timestamp, id above the marker's → uncovered; at or below → covered"
        );
    }

    /// The marker names a post that no longer exists (the newest post was
    /// deleted): nothing is newer, but the marker must **re-anchor** to the
    /// current tip — else no walk ever matches it again and every sweep
    /// re-pages the whole history forever.
    #[tokio::test]
    async fn a_deleted_newest_post_re_anchors_the_marker_without_staging() {
        let pages = Pages::of(vec![vec![
            row("bb", 2_000, "now the newest"),
            row("aa", 1_000, "oldest"),
        ]]);
        // "cc" at 3000 was deleted; everything on the wire is covered.
        let marker = posts_corpus_marker(3_000, "cc");
        let plan = plan_posts_walk(&pages, Some(&marker)).await;
        let PostsWalkPlan::Stage { posts, marker } = plan else {
            panic!("a stale marker must re-anchor via an empty Stage");
        };
        assert!(posts.is_empty(), "everything on the wire is covered");
        assert_eq!(
            parse_posts_corpus_marker(&marker),
            Some((2_000, "bb".to_string()))
        );
    }

    /// A page failure mid-walk abandons: nothing staged, marker unmoved, so
    /// the next sweep retries the same territory. Staging the prefix would be
    /// harmless; *advancing the marker over unpaged territory* is what would
    /// hide the tail from every later walk.
    #[tokio::test]
    async fn a_mid_walk_page_failure_abandons_with_the_marker_unmoved() {
        let first = PostsListReply {
            posts: vec![row("bb", 2_000, "newest")],
            cursor_created_at: Some(2_000),
            cursor_post_id: Some("bb".into()),
            extra: Default::default(),
        };
        let pages = Pages::new(vec![Ok(first), Err("the nest went away".into())]);
        let plan = plan_posts_walk(&pages, None).await;
        assert_eq!(plan, PostsWalkPlan::Abandoned);
    }

    /// An actor with no posts: one RPC, nothing staged, no marker minted —
    /// `Unchanged` reads an absent corpus honestly.
    #[tokio::test]
    async fn an_empty_corpus_is_unchanged_not_an_error() {
        let pages = Pages::of(vec![vec![]]);
        assert_eq!(
            plan_posts_walk(&pages, None).await,
            PostsWalkPlan::Unchanged
        );
    }

    /// A corrupt marker is *unknown*, never *unchanged*: the walk pages
    /// everything (the guard makes that free of republish), because early-
    /// stopping on garbage would trust a position nobody wrote.
    #[tokio::test]
    async fn a_corrupt_marker_reads_as_unknown_and_pages_everything() {
        let pages = Pages::of(vec![vec![
            row("bb", 2_000, "newest"),
            row("aa", 1_000, "old"),
        ]]);
        let plan = plan_posts_walk(&pages, Some(b"not:a:number")).await;
        assert_eq!(staged_ids(&plan), vec!["bb", "aa"]);
    }

    // ── the files walk planner ─────────────────────────────────────────────
    //
    // Same shape and same reason as the two suites above: the walk's decisions —
    // the name→stable-id join, and the three separate reasons a row is skipped —
    // are unit-testable through the `FilesEnumeration` seam, without a nest and
    // without key custody. The join is the one that matters most: it is what
    // makes a set RENAME survivable, and getting it wrong is silent (every file
    // in the set re-indexes under a new identity and its old docs are orphaned).

    fn set_summary(id: i64, name: &str) -> fauna_protocol::folders::FolderSummary {
        fauna_protocol::folders::FolderSummary {
            id,
            name: name.to_string(),
            retention_policy: None,
            cached_snapshot_count: 0,
            cached_total_bytes: 0,
            cached_last_snapshot_at: None,
            include_paths: None,
            exclude_paths: None,
            // The durable join key: a digest over the name, which the nest stores
            // and both planes project. Stable under a rename only because the
            // *nest* re-stamps it — what the walk relies on is that the two
            // planes agree at any one moment, and that the answer it keeps is the
            // `id`.
            name_hash: Some(fauna_protocol::ByteBuf::from(vec![id as u8; 32])),
            ..Default::default()
        }
    }

    fn media_item(set_name: &str, set_hash: Option<u8>, path: &str, hash: u8) -> MediaItem {
        MediaItem {
            folder: set_name.to_string(),
            path: path.to_string(),
            size_bytes: 1,
            updated_at: 1_700_000_000,
            thumbnail_hash: None,
            source_online: true,
            path_sealed: None,
            path_hash: Some(fauna_protocol::ByteBuf::from(vec![hash; 32])),
            folder_sealed: None,
            folder_hash: set_hash.map(|h| fauna_protocol::ByteBuf::from(vec![h; 32])),
            ..Default::default()
        }
    }

    use fauna_protocol::media::{MediaItem, MediaListReply};

    /// A fake enumeration: a fixed set list, a queue of page results, and a
    /// render that echoes the plaintext path unless the caller marked the set
    /// unrenderable (which is `SealedLabelRender::Omit` — the seat holds no key).
    struct Files {
        sets: Result<Vec<fauna_protocol::folders::FolderSummary>, String>,
        pages: std::sync::Mutex<std::collections::VecDeque<Result<MediaListReply, String>>>,
        unrenderable: Vec<String>,
    }

    impl Files {
        fn new(
            sets: Vec<fauna_protocol::folders::FolderSummary>,
            pages: Vec<Result<MediaListReply, String>>,
        ) -> Self {
            Self {
                sets: Ok(sets),
                pages: std::sync::Mutex::new(pages.into()),
                unrenderable: Vec::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl FilesEnumeration for Files {
        async fn list_sets(&self) -> Result<Vec<fauna_protocol::folders::FolderSummary>, String> {
            self.sets.clone()
        }
        async fn page(&self, _cursor: Option<String>) -> Result<MediaListReply, String> {
            self.pages
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(MediaListReply::default()))
        }
        async fn render_path(&self, set_name: &str, item: &MediaItem) -> Option<String> {
            if self.unrenderable.iter().any(|s| s == set_name) {
                return None;
            }
            Some(item.path.clone())
        }
    }

    fn one_page(items: Vec<MediaItem>) -> Result<MediaListReply, String> {
        Ok(MediaListReply {
            items,
            next_cursor: None,
            cursor_version: fauna_protocol::media::MEDIA_LIST_CURSOR_V2,
            signer_certs: Vec::new(),
            extra: Default::default(),
        })
    }

    fn staged(plan: &FilesWalkPlan) -> Vec<(i64, String)> {
        match plan {
            FilesWalkPlan::Stage { files, .. } => files
                .iter()
                .map(|f| (f.folder_id, f.path.clone()))
                .collect(),
            FilesWalkPlan::Nothing => Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_files_walk_drains_every_page_and_stamps_the_stable_set_id() {
        let files = Files::new(
            vec![set_summary(42, "photos")],
            vec![
                Ok(MediaListReply {
                    items: vec![media_item("photos", Some(42), "a/one.txt", 1)],
                    next_cursor: Some("c1".into()),
                    cursor_version: fauna_protocol::media::MEDIA_LIST_CURSOR_V2,
                    signer_certs: Vec::new(),
                    extra: Default::default(),
                }),
                one_page(vec![media_item("photos", Some(42), "b/two.txt", 2)]),
            ],
        );
        let plan = plan_files_walk(&files).await;
        assert_eq!(
            staged(&plan),
            vec![(42, "a/one.txt".to_string()), (42, "b/two.txt".to_string())],
            "both pages must be drained — a walk that stops at the first page is a \
             silently partial index"
        );
        assert!(
            matches!(plan, FilesWalkPlan::Stage { partial: false, .. }),
            "a drain that reached the end is not partial"
        );
    }

    /// **The load-bearing test of this arm.** `MediaItem` names its set by
    /// *name*, which the user may rename at any moment; the doc identity must be
    /// the set's stable `id`. Joining on `name_hash` — which both planes project
    /// and the nest re-stamps together — is what makes a rename cost nothing:
    /// the same file, in the same set, under a new name, still stamps set id 42.
    ///
    /// Mutation this reddens under: keying `IndexableFile::folder_id` off the
    /// set *name* (or off `name_hash` itself) instead of the joined `id`.
    #[tokio::test]
    async fn a_set_rename_does_not_move_a_files_identity() {
        let before = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![media_item("photos", Some(42), "one.txt", 1)])],
        ))
        .await;
        // The user renamed the set. The nest re-stamps both planes together, so
        // the item's `folder` and the summary's `name` move in lockstep — and
        // the row id does not.
        let after = plan_files_walk(&Files::new(
            vec![set_summary(42, "holiday-pics")],
            vec![one_page(vec![media_item(
                "holiday-pics",
                Some(42),
                "one.txt",
                1,
            )])],
        ))
        .await;
        let (FilesWalkPlan::Stage { files: a, .. }, FilesWalkPlan::Stage { files: b, .. }) =
            (&before, &after)
        else {
            panic!("both walks stage");
        };
        assert_eq!(
            (a[0].folder_id, &a[0].path_hash_hex),
            (b[0].folder_id, &b[0].path_hash_hex),
            "a set rename must leave every file identity in it untouched, or the \
             whole set re-indexes and its old docs are orphaned"
        );
    }

    /// A row this walk cannot give a durable identity is skipped, never staged
    /// under a guessed one — a doc minted under the wrong key is one no later
    /// walk can ever match or supersede.
    #[tokio::test]
    async fn a_row_whose_set_is_not_in_the_listing_is_skipped() {
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![
                media_item("photos", Some(42), "keep.txt", 1),
                media_item("gone", Some(9), "drop.txt", 2),
            ])],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(42, "keep.txt".to_string())]);
    }

    /// `path_hash` absent means this reader is not the set's label audience.
    /// No identity half, no doc — and crucially the id is left **unguarded**,
    /// so a later walk that does get the pair stages it.
    #[tokio::test]
    async fn a_row_without_a_path_hash_is_skipped() {
        let mut naked = media_item("photos", Some(42), "drop.txt", 2);
        naked.path_hash = None;
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![
                media_item("photos", Some(42), "keep.txt", 1),
                naked,
            ])],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(42, "keep.txt".to_string())]);
    }

    /// A hash of the wrong width is the same skip: better an unindexed row a
    /// later walk retries than a short id nothing will ever match.
    #[tokio::test]
    async fn a_malformed_path_hash_is_skipped_rather_than_truncated() {
        let mut bad = media_item("photos", Some(42), "drop.txt", 2);
        bad.path_hash = Some(fauna_protocol::ByteBuf::from(vec![7u8; 8]));
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![
                media_item("photos", Some(42), "keep.txt", 1),
                bad,
            ])],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(42, "keep.txt".to_string())]);
    }

    /// The ruled degrade for a set this seat holds no keys for (a set shared
    /// *with* the user on a seat with no injected resolver): skip the row, leave
    /// it unguarded, let a seat that can open it stage it.
    #[tokio::test]
    async fn a_row_whose_label_this_seat_cannot_open_is_skipped() {
        let mut files = Files::new(
            vec![set_summary(42, "mine"), set_summary(7, "theirs")],
            vec![one_page(vec![
                media_item("mine", Some(42), "keep.txt", 1),
                media_item("theirs", Some(7), "sealed.txt", 2),
            ])],
        );
        files.unrenderable = vec!["theirs".to_string()];
        let plan = plan_files_walk(&files).await;
        assert_eq!(staged(&plan), vec![(42, "keep.txt".to_string())]);
    }

    /// website-enabled folders are public-by-design plaintext and belong to backend 1;
    /// reserved `__` sets are structurally out. The nest filters both, so this is
    /// the client's independent second guard — a set that slipped through would
    /// otherwise be indexed under the user's own master key.
    #[tokio::test]
    async fn public_and_reserved_sets_are_excluded_from_the_join() {
        let plan = plan_files_walk(&Files::new(
            vec![
                fauna_protocol::folders::FolderSummary {
                    audience: "public".into(),
                    ..set_summary(1, "site")
                },
                set_summary(2, "__index"),
                set_summary(3, "docs"),
            ],
            vec![one_page(vec![
                media_item("site", Some(1), "index.html", 1),
                media_item("__index", Some(2), "seg.idx", 2),
                media_item("docs", Some(3), "notes.md", 3),
            ])],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(3, "notes.md".to_string())]);
    }

    /// **The append flavour of the abandon branch.** Unlike posts and contacts
    /// there is nothing to withhold: every doc is guarded individually and there
    /// is no marker to mis-advance, so a prefix is strictly better than nothing.
    /// It is still *reported* as partial — the distinction a future incremental
    /// cursor would have to respect.
    #[tokio::test]
    async fn a_failed_page_stages_the_prefix_and_reports_it_partial() {
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![
                Ok(MediaListReply {
                    items: vec![media_item("photos", Some(42), "one.txt", 1)],
                    next_cursor: Some("c1".into()),
                    cursor_version: fauna_protocol::media::MEDIA_LIST_CURSOR_V2,
                    signer_certs: Vec::new(),
                    extra: Default::default(),
                }),
                Err("transport died".into()),
            ],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(42, "one.txt".to_string())]);
        assert!(matches!(plan, FilesWalkPlan::Stage { partial: true, .. }));
    }

    /// No set list, no durable identities. Staging under the *name* instead would
    /// mint docs a later walk could never match — so the walk yields entirely
    /// rather than indexing under a key it knows is wrong.
    #[tokio::test]
    async fn a_failed_set_list_stages_nothing() {
        let files = Files {
            sets: Err("offline".into()),
            pages: std::sync::Mutex::new(
                vec![one_page(vec![media_item("photos", Some(42), "one.txt", 1)])].into(),
            ),
            unrenderable: Vec::new(),
        };
        assert_eq!(plan_files_walk(&files).await, FilesWalkPlan::Nothing);
    }

    /// An empty corpus stages nothing — and, File having no corpus marker,
    /// records nothing either. The next walk simply drains again.
    #[tokio::test]
    async fn an_empty_corpus_stages_nothing() {
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![])],
        ))
        .await;
        assert_eq!(plan, FilesWalkPlan::Nothing);
    }

    /// A set whose name rests plaintext (no seal stamped) ships no `folder_hash`
    /// on its rows — the nest sends the hash only paired with the seal, even
    /// though the set list carries the set's `name_hash` (the boot pass stamps
    /// every set) — so the join falls back to the plaintext name, which on that
    /// plane is unscrubbed. Covered because losing it would leave every such
    /// set out of the File index for its own label audience.
    #[tokio::test]
    async fn the_join_falls_back_to_the_plaintext_name_on_a_set_with_no_name_seal() {
        let plan = plan_files_walk(&Files::new(
            vec![set_summary(42, "photos")],
            vec![one_page(vec![media_item("photos", None, "one.txt", 1)])],
        ))
        .await;
        assert_eq!(staged(&plan), vec![(42, "one.txt".to_string())]);
    }

    // ── the drain-side dispatcher must report a Welcome ingest failure
    // through the same door the push arm uses ─────────────────────────────────────────────

    /// A `ConversationsRpc` that errors on everything but `channel_fetch` — a
    /// fresh session's Welcome ingest is purely local MLS processing and never
    /// legitimately reaches any of these; an unexpectedly-reached call fails
    /// loudly rather than silently no-opping. Mirrors
    /// `fauna-conversations/tests/common::SilentNest`, which this crate cannot
    /// import (a different crate's test-binary-local module).
    struct NoRpcNest;

    #[async_trait::async_trait]
    impl fauna_conversations::backend::ConversationsRpc for NoRpcNest {
        async fn channel_send(
            &self,
            _c: String,
            _e: Vec<u8>,
            _expect_no_commit_since: Option<i64>,
            _attachment_refs: Vec<String>,
        ) -> Result<i64, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn channel_send_remote(
            &self,
            _c: String,
            _u: String,
            _e: Vec<u8>,
            _expect_no_commit_since: Option<i64>,
            _attachment_refs: Vec<String>,
        ) -> Result<i64, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        /// The receive loop's channel sweep calls this unconditionally on
        /// construction paths that run one; the ingest itself does not.
        async fn channel_fetch(
            &self,
            _c: String,
            _a: i64,
            _l: i64,
            _h: Option<String>,
        ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
            Ok(vec![])
        }
        async fn keypackage_count(&self, _a: String) -> Result<u64, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn actor_by_handle(
            &self,
            _h: String,
        ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn actor_by_handle_remote(
            &self,
            _d: String,
            _l: String,
        ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn keypackage_fetch(
            &self,
            _a: String,
            _p: Option<String>,
        ) -> Result<Option<Vec<u8>>, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn keypackage_upload(&self, _p: Vec<Vec<u8>>, _l: bool) -> Result<u64, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn welcome_deliver(
            &self,
            _r: String,
            _c: String,
            _w: Vec<u8>,
            _k: WelcomeChannelKind,
            _p: Option<String>,
        ) -> Result<(), ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn blob_put(
            &self,
            _c: String,
            _h: Option<String>,
            _s: String,
            _b: Vec<u8>,
        ) -> Result<(), ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
        async fn blob_get(
            &self,
            _c: String,
            _h: Option<String>,
            _s: String,
        ) -> Result<Option<Vec<u8>>, ConvRpcError> {
            Err(ConvRpcError::Rejected {
                message: "not reached by a Welcome-ingest-only test".into(),
            })
        }
    }

    /// A fresh, unlinked session with a fresh in-memory MLS engine — its own
    /// identity, no prior key packages published anywhere.
    fn fresh_apply_session(seed: u8) -> (SessionInboxApply, std::sync::Arc<ConversationsSession>) {
        let engine = std::sync::Arc::new(
            fauna_mls::engine::MlsEngine::new_in_memory(
                fauna_core::identity::ActorKeypair::generate(),
            )
            .unwrap(),
        );
        let self_actor = engine.identity_actor_id();
        let session = ConversationsSession::from_manager(
            fauna_conversations::manager::ConversationsManager::new(),
            engine,
            std::sync::Arc::new(NoRpcNest)
                as std::sync::Arc<dyn fauna_conversations::backend::ConversationsRpc>,
            format!("seed-{seed}@example.com"),
            self_actor,
            None,
        );
        (
            SessionInboxApply {
                session: session.clone(),
            },
            session,
        )
    }

    /// Pin: the drain-side
    /// dispatcher must report a genuine ingest fault through
    /// `report_welcome_ingest_failure` — the same door the receive loop's
    /// push arm reports through — not flatten straight to a plain string
    /// with no level at all.
    ///
    /// Red-verify: before this row's fix, `SessionInboxApply::apply_welcome`
    /// called `.map_err(|e| e.to_string())` with no reporting call in between,
    /// so this assertion failed with an empty `lines` — the malformed arm
    /// produced no line at all, which was the defect.
    #[tokio::test]
    async fn the_drain_side_apply_reports_a_malformed_welcome_at_error() {
        let (apply, _session) = fresh_apply_session(1);

        let (result, lines) = test_tracing::capture_tracing_at_info(|| {
            block_on(apply.apply_welcome(fauna_protocol::inbox::WelcomeInbox {
                // Not a decodable MLS Welcome message by construction.
                welcome_bytes: vec![0xFF, 0x00, 0xDE, 0xAD, 0xBE, 0xEF],
                channel_id: Some("ab".repeat(32)),
                ..Default::default()
            }))
        });

        assert!(result.is_err(), "garbage welcome_bytes must not apply");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("[ERROR]") && l.contains("ingest welcome")),
            "a genuine ingest fault must reach the ring at error, naming the \
             failure — the drain's own aggregate debug counter cannot \
             distinguish this from an expected outcome; got {lines:?}"
        );
    }

    /// Pin for the second arm: a Welcome addressed to a key package this
    /// device never held ([`fauna_conversations::backend::BackendError::WelcomeNotAddressedHere`])
    /// must land at `info` — off the `error` line a genuine fault produces —
    /// through the drain-side dispatcher exactly as it already does through
    /// the push arm (`fauna_client_mls_sync::orchestration`'s
    /// `a_device_launched_before_the_mint_is_not_addressed_and_the_push_arm_says_so_below_warn`,
    /// this test's sibling).
    #[tokio::test]
    async fn the_drain_side_apply_reports_a_not_addressed_welcome_at_info_not_error() {
        // A published key package this test's own apply session never sees.
        let holder = fauna_mls::engine::MlsEngine::new_in_memory(
            fauna_core::identity::ActorKeypair::generate(),
        )
        .unwrap();
        let kp = holder.generate_key_packages(1).unwrap();

        // A peer welcomes the holder's key into a fresh group.
        let peer = fauna_mls::engine::MlsEngine::new_in_memory(
            fauna_core::identity::ActorKeypair::generate(),
        )
        .unwrap();
        let (channel, welcome) = peer.create_group(&kp).unwrap();
        let welcome_bytes = welcome.to_bytes().unwrap();

        // This test's own apply session (seed 1) is a THIRD, unrelated engine —
        // it never generated the key package the Welcome addresses, so it
        // cannot join: exactly `WelcomeNotAddressedHere`, never a fault.
        let (apply, _session) = fresh_apply_session(1);

        let (result, lines) = test_tracing::capture_tracing_at_info(|| {
            block_on(apply.apply_welcome(fauna_protocol::inbox::WelcomeInbox {
                welcome_bytes,
                channel_id: Some(channel.to_string()),
                ..Default::default()
            }))
        });

        assert!(
            result.is_err(),
            "the un-addressed device must not join the group"
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("[WARN]") || l.starts_with("[ERROR]")),
            "the multi-device steady state must not reach the ring as a fault; \
             got {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("[INFO]") && l.contains("does not hold")),
            "…but it is still said once, at info, through the drain-side door; \
             got {lines:?}"
        );
    }

    // ── The cross-nest attachment UPLOAD's classification ──
    //
    // The mint arm above classifies carefully; the upload's no-verdict fallback
    // used to pass the responder's text straight through. The axis that matters
    // is not mint-vs-upload but WHO ANSWERED: our own nest, or a room's home nest
    // chosen by whoever created the room — a stranger, on an invitation.

    /// A sentence a hostile home nest might answer an upload with. Recognisable,
    /// and shaped like the phishing it would be.
    const ATTACKER_TEXT: &str =
        "ACCOUNT SUSPENDED: verify your identity at https://evil.example/now";

    #[test]
    fn a_foreign_home_nests_upload_failure_never_reaches_the_users_sentence() {
        let e = fauna_nest_http::ApiError::Status {
            code: 400,
            message: ATTACKER_TEXT.into(),
        };
        let conv = classify_blob_upload_error(true, &e);
        assert!(
            matches!(conv, ConvRpcError::Transient { .. }),
            "still retryable — only the words change, never the bucket; got {conv:?}"
        );
        let shown = fauna_conversations::backend::BackendError::from(conv).user_detail();
        assert!(
            !shown.contains("evil.example") && !shown.contains("SUSPENDED"),
            "a foreign nest's response text reached the send slot verbatim: {shown:?}"
        );
        assert_eq!(
            shown,
            fauna_i18n::strings::error::send::ATTACHMENT_UPLOAD_FOREIGN_FAILED,
            "the one product sentence, not per-app prose"
        );
    }

    #[test]
    fn the_own_nests_upload_failure_keeps_its_transport_sentence() {
        // The inverse must not regress: our own nest is not a stranger, and its
        // transport sentence is exactly what the taxonomy lets `Transient` carry.
        let e = fauna_nest_http::ApiError::Status {
            code: 503,
            message: "nest is busy".into(),
        };
        let conv = classify_blob_upload_error(false, &e);
        let shown = fauna_conversations::backend::BackendError::from(conv).user_detail();
        assert!(
            shown.contains("nest is busy"),
            "own-nest behaviour must be unchanged: {shown:?}"
        );
    }

    // ── The cross-nest attachment mint's classification ──
    //
    // `classify_blob_mint_error` is a free function precisely so these can exist:
    // the minter it feeds is welded to a concrete `NestClient`, so the
    // classification could not be reached from a test while it lived inside the
    // closure. Same shape as the folder plane's `classify_mint_error`.
    //
    // What these pin is a SPLIT that was previously collapsed: every
    // `NestClientError::Rpc` became a hard 403, so a permanent refusal and an
    // unreachable home nest came out identical.

    #[test]
    fn a_foreign_member_refusal_is_rejected_and_fails_the_byte_plane_hard() {
        // The home nest's federation gate says this actor is not a member of the
        // channel from this nest (`federation_handlers.rs`'s `forbidden` arm).
        // Nothing about that changes on retry.
        let (api, conv) = classify_blob_mint_error(fauna_client::NestClientError::Rpc(
            fauna_protocol::RpcError::new("fauna.federation.forbidden", "error.authorization"),
        ));
        assert!(
            matches!(conv, ConvRpcError::Rejected { .. }),
            "a membership refusal must reach the send path as Rejected (→ BackendError::Refusal, \
             no retry), not Transient; got {conv:?}"
        );
        // The byte plane keeps its hard-fail framing: `WriteTokenBearer`'s
        // contract asks a refusal to be a 403 so the transfer does not retry.
        assert!(
            matches!(api, fauna_nest_http::ApiError::Status { code: 403, .. }),
            "a refusal must stay a 403 for the byte plane; got {api:?}"
        );
    }

    #[test]
    fn an_unreachable_home_nest_stays_transient_and_does_not_become_a_403() {
        // The inverted half of the same bug, and the one a variant-match could
        // never get right: the home nest being unreachable is reported by our own
        // nest as `internal("federation mint failed")`
        // (`conversations_handlers.rs`) — a wire `RpcError`, so the old
        // "every Rpc → 403" arm made the canonical RETRYABLE fault permanent.
        let (api, conv) = classify_blob_mint_error(fauna_client::NestClientError::Rpc(
            fauna_protocol::RpcError::new("fauna.protocol.internal", "error.unexpected"),
        ));
        assert!(
            matches!(conv, ConvRpcError::Transient { .. }),
            "an unreachable home nest is the canonical retryable fault and must stay Transient; \
             got {conv:?}"
        );
        assert!(
            !matches!(api, fauna_nest_http::ApiError::Status { code: 403, .. }),
            "a retryable mint fault must not be dressed as a 403 refusal; got {api:?}"
        );
    }

    #[test]
    fn the_refusal_carries_a_localized_sentence_not_a_wire_code() {
        // (b) of the finding: what reached `error-message` was
        // `blob_put: HTTP 403: blob.write_token.get refused: forbidden (sealed_cid <hex>)`.
        // `Rejected`'s message becomes `BackendError::Refusal` VERBATIM
        // (conversations.md § Errors & edge cases — "Constructing a `Refusal`
        // from raw English ... is the bug the variant names exist to make
        // visible"), so it must be the i18n sentence and nothing else.
        let (_api, conv) = classify_blob_mint_error(fauna_client::NestClientError::Rpc(
            fauna_protocol::RpcError::new("fauna.federation.forbidden", "error.authorization"),
        ));
        let ConvRpcError::Rejected { message } = conv else {
            panic!("expected Rejected, got {conv:?}");
        };
        assert_eq!(
            message,
            fauna_i18n::strings::error::AUTHORIZATION,
            "the refusal must render the shared localized sentence"
        );
        assert!(
            !message.contains("fauna.") && !message.contains("refused"),
            "no wire code or hand-written English may survive into the user's sentence: {message:?}"
        );
    }

    #[test]
    fn a_transport_fault_that_never_reached_a_nest_is_transient() {
        // No wire `RpcError` to classify, so `action()` does not apply and the
        // fault is retryable by construction.
        let (api, conv) = classify_blob_mint_error(fauna_client::NestClientError::WebSocket(
            "connect refused".into(),
        ));
        assert!(
            matches!(conv, ConvRpcError::Transient { .. }),
            "a connect failure is retryable; got {conv:?}"
        );
        assert!(
            matches!(api, fauna_nest_http::ApiError::Transport(_)),
            "a connect failure keeps its Transport framing; got {api:?}"
        );
    }
}

/// The native `blob_get`'s read bound, against a real loopback socket. This is
/// the one place a home nest's bytes enter the process, so the bound has to sit
/// on the read itself — a length check after the body was buffered whole would
/// already have paid for every byte a lying nest sent.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod blob_get_limit_tests {
    use super::*;
    use fauna_conversations::backend::ConvRpcError;
    use fauna_conversations::backend::ConversationsRpc as _;
    use fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT;
    use fauna_core::identity::ActorKeypair;
    use std::io::{Read, Write};

    /// Serve exactly one HTTP response on a loopback port — `head` (the status
    /// line and headers, without the closing blank line), then `body_len`
    /// bytes, then close — and return the base URL a `blob_get` names as the
    /// channel's home nest.
    fn serve_once(head: String, body_len: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    return;
                }
                request.push(byte[0]);
            }
            // A reader that stops early closes its end; a failed write is the
            // expected outcome then, not a fixture fault.
            let _ = stream.write_all(format!("{head}\r\n\r\n").as_bytes());
            let _ = stream.write_all(&vec![0u8; body_len]);
        });
        format!("http://127.0.0.1:{port}")
    }

    async fn blob_get_from(home: String) -> Result<Option<Vec<u8>>, ConvRpcError> {
        let nest = NestClient::new("ws://127.0.0.1:0/ws".into(), ActorKeypair::generate());
        NestConversationsRpc::with_foreign_http(nest, reqwest::Client::new())
            .blob_get("00".repeat(32), Some(home), "ab".repeat(32))
            .await
    }

    /// The bound is inclusive: a body exactly at the limit is an attachment.
    #[tokio::test]
    async fn blob_get_reads_a_body_exactly_at_the_inline_blob_limit() {
        let home = serve_once(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {INLINE_BLOB_BODY_LIMIT}\r\nConnection: close"
            ),
            INLINE_BLOB_BODY_LIMIT,
        );
        let body = blob_get_from(home)
            .await
            .expect("an at-limit body is read")
            .expect("and present");
        assert_eq!(body.len(), INLINE_BLOB_BODY_LIMIT);
    }

    /// A body DECLARED past the limit is refused on the declaration alone,
    /// before a byte of it is read, and refused as a rejection: the same nest
    /// serves the same bytes on every retry. The response carries none of the
    /// bytes it declares, so a read would end in a transport fault, never this
    /// rejection — only the declared-length refusal can answer it. A body that
    /// did carry them would also trip the running total below, and could not
    /// tell the two checks apart.
    #[tokio::test]
    async fn blob_get_refuses_a_body_declared_over_the_inline_blob_limit() {
        let over = INLINE_BLOB_BODY_LIMIT + 1;
        let home = serve_once(
            format!("HTTP/1.1 200 OK\r\nContent-Length: {over}\r\nConnection: close"),
            0,
        );
        let err = blob_get_from(home)
            .await
            .expect_err("a body declared past the limit is refused");
        assert!(
            matches!(err, ConvRpcError::Rejected { .. }),
            "not a retryable fault; got {err:?}"
        );
    }

    /// A body that declares no length and simply RUNS past the limit is cut off
    /// at the limit — the case a `Content-Length` check alone would miss.
    #[tokio::test]
    async fn blob_get_stops_reading_an_undeclared_body_past_the_inline_blob_limit() {
        let home = serve_once(
            "HTTP/1.1 200 OK\r\nConnection: close".into(),
            INLINE_BLOB_BODY_LIMIT + 1,
        );
        let err = blob_get_from(home)
            .await
            .expect_err("a body running past the limit is refused");
        assert!(
            matches!(err, ConvRpcError::Rejected { .. }),
            "not a retryable fault; got {err:?}"
        );
    }
}

/// The cross-nest attachment upload's "who answered" classification is pinned
/// directly against literal bits in `mod tests` above:
/// `a_foreign_home_nests_upload_failure_never_reaches_the_users_sentence`,
/// but nothing drove it through the real call site — a regression that kept
/// dialing the foreign home nest in [`NestConversationsRpc::blob_put`] but
/// passed the wrong `foreign` bit to `classify_blob_upload_error` would not
/// have reddened either of those pins. This module drives the real call
/// against a stub home nest that answers the upload with a recognisable
/// attacker sentence.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod blob_put_foreign_upload_tests {
    use super::*;
    use fauna_conversations::backend::ConversationsRpc as _;
    use fauna_core::identity::ActorKeypair;
    use std::io::{Read, Write};

    /// A sentence a hostile home nest might answer an upload with —
    /// deliberately the same shape as the pure-classifier pin's own
    /// `ATTACKER_TEXT` (`mod tests`, above); this module keeps its own copy
    /// rather than widen that one's visibility for a single shared literal.
    const ATTACKER_TEXT: &str =
        "ACCOUNT SUSPENDED: verify your identity at https://evil.example/now";

    /// Serve exactly one HTTP response to a POST, reading the full declared
    /// request body before replying — a response ahead of the body would wedge
    /// the client's still-writing socket into a transport fault, never the
    /// classified refusal this test pins. `head`'s status must never be `401`:
    /// a `401` is [`fauna_nest_http::ReqwestNestContentApi::send`]'s
    /// stale-bearer signal, which re-mints and retries on a SECOND connection
    /// this one-shot listener never accepts (`content.rs:175-187`). Same
    /// loopback-listener shape as `blob_get_limit_tests::serve_once`, above,
    /// extended to drain a request body.
    fn serve_once_post(head: String, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    return;
                }
                request.push(byte[0]);
            }
            let content_length: usize = String::from_utf8_lossy(&request)
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            let mut drained = vec![0u8; content_length];
            let _ = stream.read_exact(&mut drained);
            let _ = stream.write_all(format!("{head}\r\n\r\n{body}").as_bytes());
        });
        format!("http://127.0.0.1:{port}")
    }

    /// Pin: the foreign upload's
    /// "who answered" bit reaches `classify_blob_upload_error` correctly all
    /// the way from the real `blob_put` call site, not only in the direct pin.
    ///
    /// Red-verify: before this row's fix, mutating the call site's
    /// `classify_blob_upload_error(home_key.is_some(), &e)` to
    /// `classify_blob_upload_error(false, &e)` reddened neither existing pin —
    /// this test reddens on exactly that mutation, because the mutated call
    /// classifies as same-nest and the mock home nest's attacker sentence
    /// becomes the `Transient` sentence verbatim.
    #[tokio::test]
    async fn a_foreign_home_nests_upload_failure_never_reaches_blob_put_callers() {
        let home = serve_once_post(
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close",
                ATTACKER_TEXT.len()
            ),
            ATTACKER_TEXT,
        );
        let channel_id_hex = "ab".repeat(32);

        let nest = NestClient::new("ws://127.0.0.1:0/ws".into(), ActorKeypair::generate());
        let rpc = NestConversationsRpc::with_foreign_http(nest, reqwest::Client::new());
        // Skip the mint — a WebSocket RPC this loopback listener cannot answer
        // (`lib.rs:1741-1742`) — by seeding the cache it would have populated,
        // keyed exactly as the call site keys it (`lib.rs:2712-2716`).
        rpc.foreign_bearers.lock().unwrap().insert(
            (home.clone(), channel_id_hex.clone()),
            Arc::new(
                fauna_client::write_token_bearer::WriteTokenBearer::from_minter(|| async {
                    Ok::<(String, u64), fauna_nest_http::ApiError>((
                        "static-write-token".to_string(),
                        u64::MAX,
                    ))
                }),
            ),
        );

        let (result, lines) = test_tracing::capture_tracing_at_info_async(rpc.blob_put(
            channel_id_hex,
            Some(home),
            "cd".repeat(32),
            b"sealed bytes".to_vec(),
        ))
        .await;

        let err = result.expect_err("a hostile home nest's refusal must fail the send");
        assert!(
            lines.iter().any(|l| l.contains(ATTACKER_TEXT)),
            "the responder's text must actually have reached this client, or the \
             assertion below is vacuous; got {lines:?}"
        );
        let shown = fauna_conversations::backend::BackendError::from(err).user_detail();
        assert!(
            !shown.contains(ATTACKER_TEXT) && !shown.contains("evil.example"),
            "a foreign nest's response text reached the send slot the caller sees: {shown:?}"
        );
        assert_eq!(
            shown,
            fauna_i18n::strings::error::send::ATTACHMENT_UPLOAD_FOREIGN_FAILED,
            "the one product sentence, not the responder's own words"
        );
    }
}

/// The File arm's reader seat carries the attested predecessor ids its launcher
/// was handed (writer-signed change records, ruling (8)(b) source (ii)), so the
/// index walk and the query-time drain admit a retired identity's row with no
/// `fauna.recovery.succession.lookup`. The judge's own no-lookup pin is
/// `fauna_client_sync::row_judge::tests::a_successors_inherited_media_lists_and_carries_who_signed_it`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod file_seat_predecessor_tests {
    use super::*;
    use fauna_core::identity::ActorKeypair;

    /// Mutation: drop `predecessors,` from [`file_reader_seat`] and this reds.
    #[test]
    fn the_file_reader_seat_carries_the_handed_predecessor_ids() {
        let nest = NestClient::new("ws://127.0.0.1:0/ws".into(), ActorKeypair::generate());
        let ids = vec![[3u8; 32], [4u8; 32]];
        let seat = file_reader_seat(&nest, None, ids.clone());
        assert_eq!(seat.predecessors, ids);
        assert!(
            file_reader_seat(&nest, None, Vec::new())
                .predecessors
                .is_empty()
        );
    }
}
