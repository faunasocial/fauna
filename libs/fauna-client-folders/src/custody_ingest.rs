//! The nest-backed **member content-key custody-ingest** seam (Phase 0 — the read
//! leg; `docs/goal/ui/folders.md` § Sharing) — the read counterpart of the
//! owner-side [`crate::orchestration::FoldersAuthor`] custody writes.
//!
//! The M2 model already holds each member's copy of a shared set's content-key
//! bundle in their own custody — the account plane's `fauna.state.folder-keys`,
//! reached through a [`FolderKeyStore`]
//! (`docs/goal/architecture/mls-group-key-material.md` § M2). This seam populates
//! it so a *member* (not just the owner) can decrypt the set's content. The
//! conversations session drives the ingest — it holds the `MlsEngine` the open
//! step needs — and splits **around** the open: it fetches the owner's sealed
//! envelope through [`FolderCustodySink::fetch_sealed_envelope`], opens it via
//! `MlsEngine::open_content_key_envelope` at the group's current epoch, then folds
//! the generations into the member's own custody + persists through
//! [`FolderCustodySink::merge_and_persist`].
//!
//! Lives here (not in `fauna-client-conversations`, where the other `Nest*`
//! conversation seams sit) because it needs [`FoldersClient`] +
//! [`crate::custody::merge_received_keys`], and `fauna-client-folders` already
//! depends on `fauna-client-conversations` — the reverse edge would be a
//! dependency cycle. It is the natural home regardless: the read twin of the
//! owner-side custody writer that already lives in this crate. `mls`-gated
//! (needs `ChannelId`); generic over [`RpcRequester`] like every other `Nest*`
//! seam in this crate (native `Arc<NestClient>`, wasm `WsRpcClient`) — the read
//! side never touches crypto (the MLS open runs in the session's own engine),
//! so nothing here is platform-specific.

use std::sync::Arc;

use async_trait::async_trait;
use fauna_conversations::backend::{FolderCustodySink, FolderEnvelopeOwner};
use fauna_core::folder_keys::{FolderKeyResolver, FolderRef, ResolvedCustody, ResolvedFolderKeys};
use fauna_core::identity::ActorKeypair;
use fauna_mls::types::ChannelId;
use fauna_protocol::folders::ContentKeyGetRequest;
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::FoldersClient;
use crate::custody;
use crate::key_reader::{FolderKeyReader, FolderKeyStore, update};

/// The nest-backed member content-key custody-ingest seam. Wired to the two
/// transports the session's ingest driver needs but this crate reaches:
/// `fauna.folders.list` + `fauna.folders.content_key.get` (resolve the set
/// name from the member roster, then fetch the owner's sealed envelope) and the
/// custody store (join the opened bundle into this member's own custody).
/// The MLS-open *between* the two runs in the session's own engine, so this seam
/// never touches crypto — every app (macOS / iOS / Windows / Android via
/// `fauna-ffi`, linux + tui directly, web via `fauna-wasm`) ingests member
/// custody identically with no per-app glue, the read twin of
/// `fauna_client_conversations::NestFolderGate`.
/// Held by the `ConversationsSession` via `set_folder_custody_sink`.
pub struct NestFolderCustodySink<R: RpcRequester> {
    folders: FoldersClient<R>,
    custody: Arc<dyn FolderKeyStore>,
    observer: Option<Arc<dyn FolderCustodyObserver>>,
    /// What the throttled re-fetch check last found ([`Self::do_refetch_owed`]).
    refetch: std::sync::Mutex<RefetchCheck>,
    /// The least time between two re-fetch checks, in microseconds.
    refetch_check_interval: u64,
}

/// The least time between two of a sink's re-fetch checks
/// (`writer-signed-change-records.md` ruling (11)(b)). The check is one roster
/// read for every set the member holds, so the interval is what a quiet
/// session pays for the two extra triggers — one list call a minute, where a
/// check per poll tick would be one per set per tick — and the longest a
/// member signs under a retired nonce after an owner's re-mint that advanced
/// no epoch.
const REFETCH_CHECK_INTERVAL_MICROS: u64 = 60_000_000;

/// The re-fetch check's memory, RAM-only like the driver's "custody is
/// current" marks: a launch re-fetches every set anyway.
#[derive(Default)]
struct RefetchCheck {
    /// When the last check started; `None` before the first.
    checked_at: Option<u64>,
    /// The channels a check found owed and the driver has not asked about
    /// since — each spent by its ask.
    owed: std::collections::HashSet<[u8; 32]>,
    /// The engine host's request stamp each channel carried at the last check
    /// that saw it (`None`: no request yet). A channel's first sighting owes
    /// nothing: a request older than this session was answered by the
    /// launch's own fetch.
    requests_seen: std::collections::HashMap<[u8; 32], Option<u64>>,
    /// The digest of the envelope this session last fetched for each channel
    /// ([`fauna_protocol::folder_envelope_sig::digest`]; ruling (7)(b)(ii)
    /// rule (6)(a)) — what the nest's echo is compared against. A channel not
    /// fetched yet this session owes nothing on its echo: the launch's own
    /// fetch answers it.
    fetched_digests: std::collections::HashMap<[u8; 32], [u8; 32]>,
}

/// The **custody-changed** edge — fired by [`NestFolderCustodySink`] whenever an
/// ingest actually advanced this member's own content-key custody (a first
/// ingest, or an owner rotation landing a new generation), never on a no-change
/// re-ingest.
///
/// This exists because custody reaching the store is only *half* of a member
/// staying able to read a shared set. A set's content keys are **final at engine
/// build time** (`file-sync.md`; the sync agent's `engine_stamp` keys on
/// `(mls_group_id, current generation)`), so the app must re-push the resolved
/// content-key blob for the running engine to be rebuilt under the new
/// generation. The owner's *own* rotation has always had that trigger
/// (`DataMessage::FolderContentKeyRotated` → `restart_engine_for_set`); a
/// **member** receives the rotation on the conversations poll path instead, where
/// nothing told the app anything had changed — so the member's engine kept its
/// build-time generation and every subsequent owner upload failed closed in
/// `content_open_roots` until an app restart or a fresh bind
/// (`mls-group-key-material.md` § M2 *Rotate-on-removal*).
///
/// Deliberately the narrowest possible edge — "this set's custody moved", no key
/// material, no custody handle. No app shell needs it to re-key a desktop sync
/// agent any more: the custody write this edge follows is itself the `state-fleet`
/// nudge the agent re-resolves on (`on-demand-files-shared-sets.md` § Shared sets on a
/// capability host → *One mechanism*). A client with no use for the edge
/// registers none and is unaffected.
///
/// Called from the ingest's async context and **must not block** — implementors
/// spawn or notify rather than doing work inline.
pub trait FolderCustodyObserver: Send + Sync {
    /// `channel_id` is the folder's derived channel (the custody key), not its
    /// name — a name is unique only per owner, so it cannot identify a set the
    /// caller is merely a *member* of (residual R1 (account-data-plane.md § The ratified decisions)).
    fn custody_changed(&self, channel_id: &[u8; 32]);
}

impl<R: RpcRequester + Clone> NestFolderCustodySink<R> {
    /// `nest` backs the `fauna.folders.*` reads (name resolve + envelope fetch);
    /// `custody` is the account's folder-key custody — the seat's
    /// `PlaneFolderKeys` (web: the account port's forwarder) — the same store
    /// the owner-side `FoldersAuthor` writes, so a member's other devices join
    /// the merged generations, none lost (`mls-group-key-material.md` § M2).
    pub fn new(nest: R, custody: Arc<dyn FolderKeyStore>) -> Self {
        Self {
            folders: FoldersClient::new(nest),
            custody,
            observer: None,
            refetch: std::sync::Mutex::default(),
            refetch_check_interval: REFETCH_CHECK_INTERVAL_MICROS,
        }
    }

    /// Whether the envelope fetch for `channel_id` is re-owed
    /// (`FolderCustodySink::refetch_owed`), answered from the last check and
    /// spent by the asking. Runs the check first when one is due — at most one
    /// per [`REFETCH_CHECK_INTERVAL_MICROS`], whichever channel's ask finds it
    /// due, so a quiet poll over every set costs no network call in between.
    async fn do_refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
        let now = fauna_core::data::Timestamp::now().0;
        // Stamped before the check runs: the asks for this pass's other
        // channels land while it is in flight and must not each start one.
        let due = {
            let mut check = self.refetch.lock().unwrap_or_else(|e| e.into_inner());
            let due = check
                .checked_at
                .is_none_or(|at| now.saturating_sub(at) >= self.refetch_check_interval);
            if due {
                check.checked_at = Some(now);
            }
            due
        };
        if due {
            self.check_refetch_owed().await;
        }
        self.refetch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .owed
            .remove(channel_id)
    }

    /// One re-fetch check over every set this member holds
    /// (`writer-signed-change-records.md` ruling (11)(b)) — the two triggers
    /// that advance no epoch:
    ///
    /// - **the pushed nonce changed**: a member row whose nonce echo
    ///   ([`fauna_protocol::folders::FolderSummary::set_nonce`]) is not the
    ///   nonce custody holds for its channel. The echo is only ever a reason
    ///   to fetch — the nonce itself still comes from the owner-signed
    ///   envelope. A row that echoes none asserts nothing.
    /// - **the envelope's digest echo moved** (ruling (7)(b)(ii) rule (6)(a)):
    ///   a member row whose
    ///   [`fauna_protocol::folders::FolderSummary::envelope_digest`] is not the
    ///   digest of the envelope this session last fetched for its channel —
    ///   an owner's re-publish that moved no nonce and advanced no epoch (a
    ///   serve flip, a rotation's re-publish). Only ever a reason to fetch; a
    ///   channel not fetched yet this session, or a row that echoes none,
    ///   owes nothing.
    /// - **a row was refused `signature_invalid`, or admitted under a
    ///   generation custody lacks**: the engine host's request stamp for the
    ///   channel ([`FolderKeyReader::request_refetch`]) moved since the last
    ///   check that saw it. This is what reaches a foreign set, which has no
    ///   roster row here to echo anything.
    ///
    /// Best-effort like every sink read: an unreadable custody finds nothing,
    /// an unreadable roster leaves the echo arms for the next check.
    async fn check_refetch_owed(&self) {
        let Ok(cfg) = self.custody.load().await else {
            return;
        };
        let roster = match self.folders.list_owned_and_shared_wire().await {
            Ok(reply) => reply.folders,
            Err(_) => Vec::new(),
        };
        let mut channels: Vec<[u8; 32]> = Vec::new();
        let mut moved: Vec<[u8; 32]> = Vec::new();
        let mut digest_echoes: Vec<([u8; 32], [u8; 32])> = Vec::new();
        for row in &roster {
            // A set the caller owns takes its nonce from its own custody.
            if row.role.as_deref() != Some("member") {
                continue;
            }
            let Some(channel) = row
                .mls_group_id
                .as_deref()
                .and_then(|g| hex::decode(g.trim()).ok())
                .map(|raw| ChannelId::from_group_id(&raw).0)
            else {
                continue;
            };
            channels.push(channel);
            let echo = row
                .set_nonce
                .as_ref()
                .and_then(|n| <[u8; 32]>::try_from(n.as_slice()).ok());
            if let Some(echo) = echo
                && custody::set_nonce_for_channel(&cfg, &channel) != Some(echo)
            {
                moved.push(channel);
            }
            if let Some(digest) = row
                .envelope_digest
                .as_ref()
                .and_then(|d| <[u8; 32]>::try_from(d.as_slice()).ok())
            {
                digest_echoes.push((channel, digest));
            }
        }
        channels.extend(custody::live_foreign_sets(&cfg).map(|f| f.channel_id));
        let mut requests = Vec::with_capacity(channels.len());
        for channel in channels {
            // A local read (the store's replica-local meta), never the network.
            let at = self
                .custody
                .refetch_requested_at(&channel)
                .await
                .unwrap_or(None);
            requests.push((channel, at));
        }
        let mut check = self.refetch.lock().unwrap_or_else(|e| e.into_inner());
        check.owed.extend(moved);
        for (channel, digest) in digest_echoes {
            if check
                .fetched_digests
                .get(&channel)
                .is_some_and(|fetched| *fetched != digest)
            {
                check.owed.insert(channel);
            }
        }
        for (channel, at) in requests {
            if let Some(seen) = check.requests_seen.insert(channel, at)
                && at > seen
            {
                check.owed.insert(channel);
            }
        }
    }

    /// Register the [`FolderCustodyObserver`] this sink notifies on every ingest
    /// that genuinely advanced custody. Optional: a sink without one behaves
    /// exactly as before (it only persists), which is what a client with no local
    /// sync engine wants.
    pub fn with_observer(mut self, observer: Arc<dyn FolderCustodyObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Resolve a joined folder channel back to its nest **address** — the
    /// row's `(id, name, name_hash)` `content_key.get` takes, the row first
    /// (`set-addressing.md` § The row form). The member-visible
    /// roster (`include_shared_with_me`) carries `(name_hash, mls_group_id)`;
    /// match the row whose derived `ChannelId` equals the target. `None` when
    /// the set is not (yet) in the roster — best-effort, retried next pass.
    /// This only ever runs for channels the engine has already **joined**
    /// (ingest is driven from the join + poll paths), so a rostered-but-
    /// unaccepted knock can never match.
    ///
    /// The roster is read **unrendered** (`list_owned_and_shared_wire`): this
    /// sink is what fetches the keys a label render would need, and a sealed
    /// set's row rests no plaintext name (schema 114) — its hash is the address.
    async fn resolve_address(
        &self,
        channel_id_hex: &str,
    ) -> Option<(i64, String, Option<fauna_protocol::ByteBuf>)> {
        let reply = self.folders.list_owned_and_shared_wire().await.ok()?;
        reply.folders.into_iter().find_map(|s| {
            let group_hex = s.mls_group_id.as_ref()?;
            let raw = hex::decode(group_hex.trim()).ok()?;
            (ChannelId::from_group_id(&raw).to_string() == channel_id_hex).then_some((
                s.id,
                s.name,
                s.name_hash,
            ))
        })
    }
}

// The actual seam logic, generic over `R` and written ONCE (priority #2) as
// plain inherent async methods — NOT inside the `#[async_trait]`-attributed
// trait impl below. An async-trait-generated future must prove `Send` on
// native, and `R: RpcRequester`'s AFIT methods don't carry that proof for an
// abstract `R` (only a concrete instantiation like `Arc<NestClient>` lets the
// compiler see through to a provably-`Send` future). So the trait impl is
// duplicated per concrete target (mirrors `WsRpcDevicesNest<R>` in
// `fauna-devices-machine`'s `ws_rpc.rs`) and each method there is a one-line
// delegate to the shared logic here.
/// The advisory stamps a foreign-set record held before a federated
/// content-key read — what [`NestFolderCustodySink::do_fetch_sealed_envelope`]
/// compares the reply's stamps against, so an unchanged set of stamps (the
/// steady state, once per poll per set) costs no custody write.
struct HeldForeignStamps {
    channel_id: [u8; 32],
    access: Option<String>,
    home_nest_actor_id: Option<String>,
    content_key_floor: Option<u64>,
    metadata_only_residency: Option<bool>,
    owner_handle: Option<String>,
    owner_domain: Option<String>,
}

impl<R: RpcRequester + Clone> NestFolderCustodySink<R>
where
    R::Error: RpcErrorClass,
{
    async fn do_fetch_sealed_envelope(
        &self,
        channel_id_hex: &str,
    ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
        // Same-nest first: the member-visible roster resolves the set's name.
        // A FOREIGN set (cross-nest share) has no roster row on this nest —
        // fall through to the custody foreign-set record, whose
        // `home_nest_url` routes the read through the own-nest relay
        // (`nest_url` + `channel_id`, channel-keyed — the name only resolves on
        // the home nest; Phase 2 client read-side).
        // `Some` on the foreign branch — the advisory values held before the
        // read, carried out so the refresh below can decide whether the home
        // nest's stamps actually CHANGED anything without a second config load.
        let mut foreign_ctx: Option<HeldForeignStamps> = None;
        let request = if let Some((id, name, name_hash)) =
            self.resolve_address(channel_id_hex).await
        {
            // The row the channel resolved to is the address
            // (`set-addressing.md` § The row form): beside a same-named own
            // set the name forms alone would answer the own set's envelope.
            ContentKeyGetRequest {
                name,
                name_hash,
                folder_id: Some(id),
                ..Default::default()
            }
        } else {
            let cfg = self.custody.load().await.ok()?;
            let channel_id: [u8; 32] = hex::decode(channel_id_hex.trim()).ok()?.try_into().ok()?;
            let foreign = custody::find_foreign_set(&cfg, &channel_id)?;
            let request = ContentKeyGetRequest {
                nest_url: Some(foreign.home_nest_url.clone()),
                channel_id: Some(channel_id_hex.to_string()),
                ..Default::default()
            };
            foreign_ctx = Some(HeldForeignStamps {
                channel_id,
                access: foreign.access.clone(),
                home_nest_actor_id: foreign.home_nest_actor_id.clone(),
                content_key_floor: foreign.content_key_floor,
                metadata_only_residency: foreign.metadata_only_residency(),
                owner_handle: foreign.owner_handle.clone(),
                owner_domain: foreign.owner_domain.clone(),
            });
            request
        };
        let reply = self.folders.content_key_get(request).await.ok()?; // not_published / not_found / transient → retry next pass
        // Refresh half of *Recipient-side access discovery* (`federation.md`
        // § Cross-nest): a FOREIGN read's reply carries the home nest's stamp of
        // this caller's live grant, so a promotion or demotion is picked up on
        // the ordinary commit-poll cadence — no push kind, poll-parity with
        // same-nest access changes (which have none either).
        //
        // **Guarded on a real change, deliberately.** A custody write always
        // reads and joins through the store — it cannot know the closure was a
        // no-op — so calling it per poll would write for a grant that never
        // moves. The
        // stamp is compared against the value this branch already loaded, so
        // the steady state costs zero extra round trips and zero writes.
        //
        // An absent stamp asserts nothing (a same-nest read, or no role row
        // = the implicit reader default) and must never be read as a revocation;
        // demotion is enforced fail-closed at the next mint/record. The same reply
        // refreshes the home-nest identity (byte-plane pin root), the owner's
        // cadence and the set's content-key floor (the member engine's pre-seal
        // hold arms from it — `on-demand-files-shared-sets.md` § Shared sets on a capability
        // host → *One mechanism*, question 2) — all carried by every federated
        // content-key read reply. Best-effort: a persist failure never fails the
        // envelope fetch the caller came for.
        if let Some(held) = foreign_ctx {
            let channel_id = held.channel_id;
            let access = reply.caller_access.clone();
            let home_nest_actor_id = reply.home_nest_actor_id.clone();
            let floor = reply.content_key_floor;
            let residency = reply.residency.clone();
            let stated = fauna_core::data::ForeignResidency::parse_stamp(residency.as_deref());
            // The cross-nest owner label, as this member's own nest verified
            // it — a pair or nothing (`federation-shared-folders.md` § … *The cross-nest
            // owner label*).
            let owner = reply.owner_handle.clone().zip(reply.owner_domain.clone());
            let changed = (access.is_some() && access != held.access)
                || (home_nest_actor_id.is_some() && home_nest_actor_id != held.home_nest_actor_id)
                || (floor.is_some() && floor != held.content_key_floor)
                || (stated.is_some() && stated != held.metadata_only_residency)
                || owner.as_ref().is_some_and(|(h, d)| {
                    (held.owner_handle.as_deref(), held.owner_domain.as_deref())
                        != (Some(h.as_str()), Some(d.as_str()))
                });
            if changed
                && let Err(e) = update(&*self.custody, move |cfg| {
                    custody::refresh_foreign_set_from_reply(
                        cfg,
                        &channel_id,
                        custody::ForeignReplyStamps {
                            access: access.as_deref(),
                            home_nest_actor_id: home_nest_actor_id.as_deref(),
                            content_key_floor: floor,
                            residency: residency.as_deref(),
                            owner: owner.as_ref().map(|(h, d)| (h.as_str(), d.as_str())),
                        },
                        fauna_core::data::Timestamp::now().0,
                    );
                })
                .await
            {
                tracing::debug!("foreign-set refresh failed (retry next poll): {e:#}");
            }
        }
        let blob = hex::decode(reply.sealed.trim()).ok()?;
        let channel_id: [u8; 32] = hex::decode(channel_id_hex.trim()).ok()?.try_into().ok()?;
        // The envelope this session last fetched, as the nest's digest echo
        // names it (ruling (7)(b)(ii) rule (6)(a)) — remembered at the fetch,
        // whatever the open makes of it: a failed open clears the driver's
        // mark and the next pass retries anyway. A foreign set's fetch records
        // too, harmlessly — no row echoes for it.
        self.refetch
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fetched_digests
            .insert(
                channel_id,
                fauna_protocol::folder_envelope_sig::digest(&blob),
            );
        // No envelope is ingested without a signature that verifies
        // (`writer-signed-change-records.md` ruling (11)(b)); who may move what
        // is the driver's call, against the owner its MLS state records.
        match fauna_protocol::folder_envelope_sig::verify(&blob, &channel_id) {
            Ok(signed) => Some(fauna_conversations::backend::FetchedEnvelope {
                signer: signed.signer,
                sealed: signed.sealed,
            }),
            Err(e) => {
                tracing::warn!(
                    "folder custody ingest: the stored envelope is not owner-signed — refused \
                     (retry next pass): {e}"
                );
                None
            }
        }
    }

    async fn do_merge_and_persist(
        &self,
        channel_id: &[u8; 32],
        payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
        may_move: bool,
    ) -> bool {
        let cid = *channel_id;
        // The cross-device-safe read-modify-write: load → ingest the received
        // envelope into custody (keys by the idempotent CRDT, the nonce forward
        // only with its lineage — `writer-signed-change-records.md` ruling
        // (11)(b)) → join into the store, so a concurrent device's write is
        // joined in, never clobbered — the same `key_reader::update` primitive
        // the owner-side custody writers use.
        //
        // `update` carries the ingest's own *did anything change* answer back
        // out (this fn's `bool` reports **persisted**, which is a different
        // question — a no-change re-ingest still writes and still succeeds).
        // Only a genuine advance may reach the observer: the edge drives a full
        // agent re-provision + engine restart, so firing it per poll would
        // restart a member's engine on every cadence tick. A refused envelope
        // changes nothing and answers `false` — a NON-merge, so the driver
        // retries rather than marking the channel current.
        match update(&*self.custody, move |cfg| {
            custody::ingest_received_envelope(
                cfg,
                cid,
                payload,
                may_move,
                fauna_core::data::Timestamp::now().0,
            )
        })
        .await
        {
            Ok((_, Ok(changed))) => {
                if changed && let Some(observer) = &self.observer {
                    // Fired only AFTER the persist landed: the observer's job is
                    // to re-push what the app resolves from custody, so an
                    // edge raised over an unsaved merge would push the old
                    // generation and mark the set done at the wrong one.
                    observer.custody_changed(&cid);
                }
                true
            }
            Ok((_, Err(refusal))) => {
                tracing::warn!(
                    "folder custody ingest: the envelope was refused (retry next pass): {refusal}"
                );
                false
            }
            Err(e) => {
                tracing::debug!("folder custody persist failed (retry next pass): {e:#}");
                false
            }
        }
    }

    async fn do_name_foreign_set_from_seal(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
        name_hash: &[u8],
    ) -> bool {
        let (channel_id, sealed, name_hash) = (*channel_id, sealed.to_vec(), name_hash.to_vec());
        match update(&*self.custody, move |cfg| {
            custody::name_foreign_set_from_seal(
                cfg,
                &channel_id,
                &sealed,
                &name_hash,
                fauna_core::data::Timestamp::now().0,
            )
        })
        .await
        {
            Ok((_, named)) => named,
            Err(e) => {
                tracing::debug!(
                    "foreign-set name persist failed (re-opened on next accept): {e:#}"
                );
                false
            }
        }
    }

    async fn do_record_foreign_set(&self, record: fauna_core::data::ForeignFolder) -> bool {
        // Same read-modify-write as `merge_and_persist`; the upsert is
        // idempotent (`record_foreign_set` returns false on a byte-identical
        // re-record, and the join is harmless either way).
        match update(&*self.custody, move |cfg| {
            let collided = custody::foreign_name_collides(cfg, &record);
            custody::record_foreign_set(cfg, record, fauna_core::data::Timestamp::now().0);
            collided
        })
        .await
        {
            Ok((_, collided)) => {
                if collided {
                    // Loud by contract: name-keyed
                    // lookups are first-match, so this record is shadowed and
                    // unaddressable by name until the collision dissolves. (No
                    // set name in the log — S7.)
                    tracing::warn!(
                        "foreign folder record accepted with a DISPLAY-NAME collision \
                         against another foreign set; the later record is unaddressable \
                         by name (channel-keyed reads are unaffected)"
                    );
                }
                true
            }
            Err(e) => {
                tracing::debug!(
                    "foreign-set record persist failed (re-recorded on re-accept): {e:#}"
                );
                false
            }
        }
    }

    async fn do_forget_foreign_set(&self, channel_id: &[u8; 32]) -> bool {
        let cid = *channel_id;
        match update(&*self.custody, move |cfg| {
            custody::forget_foreign_set(cfg, &cid, fauna_core::data::Timestamp::now().0);
        })
        .await
        {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!(
                    "foreign-set record forget failed (stale record is display-only): {e:#}"
                );
                false
            }
        }
    }

    /// Tri-state (see [`FolderCustodySink::foreign_home_url`]): an unreadable
    /// custody record is `Err`, never `Ok(None)`.
    async fn do_foreign_home_url(&self, channel_id: &[u8; 32]) -> Result<Option<String>, String> {
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| format!("foreign-set custody record unreadable: {e:#}"))?;
        Ok(custody::find_foreign_set(&cfg, channel_id).map(|f| f.home_nest_url.clone()))
    }

    /// One custody read for the WHOLE foreign population — the launch
    /// restore's direction, where per-channel would be one read each over a
    /// list whose same-nest majority holds no record at all. An unreadable
    /// custody yields an empty population, not an error: the seed is a
    /// best-effort recovery like every other sink read, and a channel whose
    /// slice already carries its home is unaffected either way.
    async fn do_foreign_homes(&self) -> Vec<([u8; 32], String)> {
        let Ok(cfg) = self.custody.load().await else {
            return Vec::new();
        };
        custody::live_foreign_sets(&cfg)
            .map(|f| (f.channel_id, f.home_nest_url.clone()))
            .collect()
    }
}

/// Native concrete trait impl (see the module doc above for why this is
/// duplicated per target rather than one generic `impl<R>`).
#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl FolderCustodySink for NestFolderCustodySink<Arc<fauna_client::NestClient>> {
    async fn fetch_sealed_envelope(
        &self,
        channel_id_hex: &str,
    ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
        self.do_fetch_sealed_envelope(channel_id_hex).await
    }

    async fn merge_and_persist(
        &self,
        channel_id: &[u8; 32],
        payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
        may_move: bool,
    ) -> bool {
        self.do_merge_and_persist(channel_id, payload, may_move)
            .await
    }

    async fn record_foreign_set(&self, record: fauna_core::data::ForeignFolder) -> bool {
        self.do_record_foreign_set(record).await
    }

    async fn name_foreign_set_from_seal(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
        name_hash: &[u8],
    ) -> bool {
        self.do_name_foreign_set_from_seal(channel_id, sealed, name_hash)
            .await
    }

    async fn forget_foreign_set(&self, channel_id: &[u8; 32]) -> bool {
        self.do_forget_foreign_set(channel_id).await
    }

    async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Result<Option<String>, String> {
        self.do_foreign_home_url(channel_id).await
    }

    async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
        self.do_foreign_homes().await
    }

    async fn refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
        self.do_refetch_owed(channel_id).await
    }
}

/// Wasm concrete trait impl — the browser twin of the native impl above, over
/// the SPA's `WsRpcClient` instead of `Arc<NestClient>`. Same shared logic.
#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl FolderCustodySink for NestFolderCustodySink<fauna_rpc_wasm::WsRpcClient> {
    async fn fetch_sealed_envelope(
        &self,
        channel_id_hex: &str,
    ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
        self.do_fetch_sealed_envelope(channel_id_hex).await
    }

    async fn merge_and_persist(
        &self,
        channel_id: &[u8; 32],
        payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
        may_move: bool,
    ) -> bool {
        self.do_merge_and_persist(channel_id, payload, may_move)
            .await
    }

    async fn record_foreign_set(&self, record: fauna_core::data::ForeignFolder) -> bool {
        self.do_record_foreign_set(record).await
    }

    async fn name_foreign_set_from_seal(
        &self,
        channel_id: &[u8; 32],
        sealed: &[u8],
        name_hash: &[u8],
    ) -> bool {
        self.do_name_foreign_set_from_seal(channel_id, sealed, name_hash)
            .await
    }

    async fn forget_foreign_set(&self, channel_id: &[u8; 32]) -> bool {
        self.do_forget_foreign_set(channel_id).await
    }

    async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Result<Option<String>, String> {
        self.do_foreign_home_url(channel_id).await
    }

    async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
        self.do_foreign_homes().await
    }

    async fn refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
        self.do_refetch_owed(channel_id).await
    }
}

/// The owner-side twin of the sink above — the seam the owner's folder commit
/// walk asks to re-seal an envelope the group's epoch has moved past
/// ([`FolderEnvelopeOwner`]; `mls-group-key-material.md` § Admitting a member,
/// *Envelope ordering*): [`crate::orchestration::FoldersAuthor::converge_envelope`],
/// with a fault logged and answered unsettled. Registered by the launch pass
/// that builds the owner's author (`launch_resume.rs`, the web launch face) on
/// an author built **without** the commit gate, which the converge never
/// uses — the gate is the backend itself, and the backend holding an author
/// that holds the backend would keep both alive past the session.
async fn converge_owned_envelope<R, G>(
    author: &crate::orchestration::FoldersAuthor<R, G>,
    channel_id: &[u8; 32],
) -> bool
where
    R: RpcRequester,
    R::Error: RpcErrorClass + core::fmt::Display,
    G: crate::orchestration::FolderGroupCrypto,
    G::Error: core::fmt::Display,
{
    match author.converge_envelope(channel_id).await {
        Ok(settled) => settled,
        Err(e) => {
            tracing::warn!(
                "folders: the mid-session envelope re-seal failed (retried at the next walk): {e}"
            );
            false
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl FolderEnvelopeOwner
    for crate::orchestration::FoldersAuthor<
        Arc<fauna_client::NestClient>,
        Arc<fauna_mls::engine::MlsEngine>,
    >
{
    async fn converge_envelope(&self, channel_id: &[u8; 32]) -> bool {
        converge_owned_envelope(self, channel_id).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl FolderEnvelopeOwner
    for crate::orchestration::FoldersAuthor<
        fauna_rpc_wasm::WsRpcClient,
        Arc<fauna_mls::engine::MlsEngine>,
    >
{
    async fn converge_envelope(&self, channel_id: &[u8; 32]) -> bool {
        converge_owned_envelope(self, channel_id).await
    }
}

/// The nest-backed **shared-folder content-key resolver** (Phase 0 — the read
/// leg; `folders.md` § Sharing): the read counterpart every custody-holding
/// surface consults so a **shared** set opens under content keys from custody
/// rather than the owner `BackupKey`. It resolves a set **name** → its bound
/// `mls_group_id` (from the member-visible roster) + the caller's own
/// content-key custody (through a [`FolderKeyReader`]), returning `None` for an
/// owner-only (unbound) set.
///
/// Two classes of consumer, one resolver: the Media machine's **byte** download
/// (`MediaMachine::download_file`) and the **sealed-label** renders on media
/// list / snapshot browse+diff / conflicts, which reach it through
/// [`fauna_core::label_custody::LabelCustody`]. That sharing is the ruling's own
/// logic — whoever can open a set's bytes renders its names
/// (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
///
/// Sibling of [`NestFolderCustodySink`] (same `FoldersClient` + custody
/// pattern), kept a distinct type because it serves a different seam
/// ([`FolderKeyResolver`] on the Media machine, not `FolderCustodySink` on the
/// conversations session) and resolves in the opposite direction (name → keys,
/// not channel → envelope). Built by the client's Media glue.
pub struct NestFolderKeyResolver<R: RpcRequester> {
    folders: FoldersClient<R>,
    custody: Arc<dyn FolderKeyReader>,
}

impl<R: RpcRequester + Clone> NestFolderKeyResolver<R> {
    /// `nest` backs the roster read (`fauna.folders.list`); `custody` reads the
    /// caller's own content-key custody — the seat's `PlaneFolderKeys` (web:
    /// the account port's forwarder).
    pub fn new(nest: R, custody: Arc<dyn FolderKeyReader>) -> Self {
        Self {
            folders: FoldersClient::new(nest),
            custody,
        }
    }
}

/// The pure custody decision: one roster read + one custody read → the
/// three-valued [`FolderKeyResolver`] answer. Split from the async shell so
/// the contract is unit-testable without a nest.
///
/// **A roster row addressed by `name_hash` — owned or member — is an authoritative
/// identity answer, and the foreign lookup is only reachable when NO such row
/// exists.** The previous shape collapsed "an owned row exists and is
/// UNBOUND" (a positive answer: the owner key IS its custody) with "no row at
/// all" into one miss and fell through to the foreign lookup
/// ([`custody::find_foreign_set_by_hash`] today)
/// — so a foreign record whose freely-chosen display name collided with the
/// user's own unbound set (`photos`, `documents`, …) resolved the user's own
/// set to a **stranger's roster content root**: labels sealed to the wrong
/// audience, chunk fetches routed to the stranger's nest, and the S9 scrub
/// primed to destroy plaintext under a seal nobody could open.
///
/// Corollary (deliberate): an owned/member row
/// **shadows** a same-named foreign record — the foreign set is unaddressable
/// by this name until the collision dissolves. A reader holding the set's row
/// addresses it by identity instead — [`resolve_custody_by_ref`].
pub(crate) fn resolve_custody_from(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    name_hash: &[u8; 32],
) -> anyhow::Result<ResolvedCustody> {
    if let Some(row) = custody::find_named_roster_row(roster, cfg, name_hash) {
        return custody_of_roster_row(&row, cfg);
    }
    // No roster row — a FOREIGN (cross-nest) set resolves from the member's
    // own custody record instead (written at accept time; Phase 2 client
    // read-side): identity + the home-nest base URL the byte fetch must use. A
    // name-less foreign record (an unopenable seal) is unaddressable by
    // name and stays owner-only — it resolves by channel through
    // [`resolve_custody_by_ref`].
    let Some(foreign) = custody::find_foreign_set_by_hash(cfg, name_hash) else {
        return Ok(ResolvedCustody::owner_only());
    };
    Ok(custody_of_foreign_set(foreign, cfg))
}

/// The custody of a set **by its identity** — the
/// [`FolderKeyResolver::resolve_ref`] twin of [`resolve_custody_from`]
/// (`mls-group-key-material.md` § M2 → *Resolving custody — by the set's
/// identity, never by its name*). [`FolderRef::Local`] is the roster row with
/// that `id` — no owner-first search, so a member row is reachable beside the
/// reader's own same-named set — and [`FolderRef::Foreign`] the live foreign
/// record at that channel. A ref naming neither is `Err`: a caller holding a
/// set's identity asked about THAT set, and "no such set" is not the positive
/// owner-only answer a by-name miss is. Everything after the pick is
/// [`resolve_custody_from`]'s own arm, shared.
pub(crate) fn resolve_custody_by_ref(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder_ref: &FolderRef,
) -> anyhow::Result<ResolvedCustody> {
    match folder_ref {
        FolderRef::Local(id) => {
            // (No set name or id in the message — S7.)
            let row = roster
                .iter()
                .find(|s| s.id == *id)
                .ok_or_else(|| anyhow::anyhow!("no roster row with that id"))?;
            custody_of_roster_row(&custody::named_roster_row(row.clone(), cfg), cfg)
        }
        FolderRef::Foreign(channel) => custody::find_foreign_set(cfg, channel)
            .map(|foreign| custody_of_foreign_set(foreign, cfg))
            .ok_or_else(|| anyhow::anyhow!("no live foreign set at that channel")),
    }
}

/// The custody answer for a roster row already picked and named from custody
/// ([`custody::named_roster_row`]) — the arm both resolution directions share.
fn custody_of_roster_row(
    row: &fauna_protocol::folders::FolderSummary,
    cfg: &fauna_core::data::FoldersConfig,
) -> anyhow::Result<ResolvedCustody> {
    // The custody identity through the ONE shared resolution every
    // consumer uses (`webdav-server.md` § Key model, the custody note):
    // the derived `ChannelId` for a shared set, the serve pseudo-channel
    // for a served-unshared one, `None` for a plain owner-only set. A
    // bound row whose group id cannot be decoded is corrupt state, not
    // "unbound": answering owner-only would license an owner-root seal
    // for a set that IS bound. (No set name in the message — S7.)
    let channel = crate::engine_binding::custody_channel_for(row, cfg)
        .map_err(|_| anyhow::anyhow!("bound roster row carries an undecodable mls_group_id"))?;
    let Some(channel) = channel else {
        // Owned, unbound, unserved: positively owner-only — plus, for a
        // set a since-unflagged serve window once sealed, that window's
        // generations as READ candidates (`retired_serve_custody`, the
        // read-side twin of the engine's `retired_content_keys`).
        return Ok(ResolvedCustody::OwnerOnly {
            retired_content_keys: crate::engine_binding::retired_serve_custody(cfg, row),
        });
    };
    // The raw group id rides only when there IS a group: a served-unshared
    // set is content-keyed with no group, and never fakes one.
    let mls_group_id = row
        .mls_group_id
        .as_deref()
        .map(hex::decode)
        .transpose()
        .map_err(|_| anyhow::anyhow!("bound roster row carries an undecodable mls_group_id"))?;
    // Custody: the caller's content-key generations (member custody
    // ingested at join/rotation, or the owner's own — for a served set, the
    // serve-enable genesis at the pseudo-channel). Absent →
    // content-keyed-but-unresolvable: the content-keyed-ness still rides,
    // so seals fail closed and opens fail on their own — never the
    // owner-key path (`EngineKeyBinding::{BoundKeysMissing,ServedKeysMissing}`'s
    // read-side twins).
    Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
        content_keys: custody::content_keys(cfg, &channel),
        mls_group_id,
        home_nest_url: None,
        home_nest_actor_id: None,
    }))
}

/// The custody answer for a live foreign record already picked — the arm both
/// resolution directions share.
fn custody_of_foreign_set(
    foreign: &fauna_core::data::ForeignFolder,
    cfg: &fauna_core::data::FoldersConfig,
) -> ResolvedCustody {
    ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
        mls_group_id: Some(foreign.mls_group_id.clone()),
        content_keys: custody::content_keys(cfg, &foreign.channel_id),
        home_nest_url: Some(foreign.home_nest_url.clone()),
        // The grant-delivered trust root for the byte-plane dial
        // (`refresh_foreign_set_from_reply` keeps it current from every
        // federated read reply).
        home_nest_actor_id: foreign.home_nest_actor_id.clone(),
    })
}

/// The [`fauna_client_sync::RecordSigning`] a seed-holding client's side
/// recorders (version restore, archive import, the FFI sync client) sign with:
/// the identity key directly (ruling (1)'s seed-holding host), each record's
/// nonce by folder name through this nest-backed resolver over the account's
/// `custody`. One shape for every such recorder, so none resolves a nonce its
/// own way.
pub fn record_signing<R>(
    nest: R,
    identity: &ActorKeypair,
    custody: Arc<dyn FolderKeyReader>,
) -> fauna_client_sync::RecordSigning
where
    R: RpcRequester + Clone,
    NestFolderKeyResolver<R>: FolderKeyResolver + 'static,
{
    fauna_client_sync::RecordSigning {
        signer: Arc::new(fauna_protocol::sync_writer_sig::ChangeSigner::direct(
            identity,
        )),
        set_nonce: fauna_client_sync::SetNonceSource::Resolver(Arc::new(
            NestFolderKeyResolver::new(nest, custody),
        )),
    }
}

/// The set nonce for `folder` by the same name resolution
/// [`resolve_custody_from`] uses: a roster row resolves by its custody channel
/// (a member's received copy or the owner's keyed entry), else — for the
/// caller's OWN row — the owner's live pick by name; a foreign record by its
/// channel.
pub(crate) fn resolve_set_nonce_from(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder: &str,
) -> Option<[u8; 32]> {
    custody::set_nonce_by_name(roster, cfg, folder)
}

/// [`resolve_set_nonce_from`] by the set's identity
/// ([`FolderKeyResolver::set_nonce_ref`]): the roster row with that id —
/// never the owner-first name search, so a member row answers its own nonce
/// beside the caller's same-named set — or the foreign record at a channel.
pub(crate) fn resolve_set_nonce_by_ref(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    folder_ref: &FolderRef,
) -> Option<[u8; 32]> {
    match folder_ref {
        FolderRef::Local(id) => {
            let row = roster.iter().find(|s| s.id == *id)?;
            custody::set_nonce_of_row(&custody::named_roster_row(row.clone(), cfg), cfg)
        }
        FolderRef::Foreign(channel) => custody::find_foreign_set(cfg, channel)
            .and_then(|f| custody::set_nonce_for_channel(cfg, &f.channel_id)),
    }
}

// Same shared-logic-once / per-target-trait-impl split as
// `NestFolderCustodySink` above (see that module doc for why).
impl<R: RpcRequester + Clone> NestFolderKeyResolver<R> {
    async fn do_set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set nonce: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set nonce"))?;
        Ok(resolve_set_nonce_from(&reply.folders, &cfg, folder))
    }

    async fn do_set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        // The same two reads as `do_set_nonce`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set nonce: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set nonce"))?;
        Ok(resolve_set_nonce_by_ref(&reply.folders, &cfg, folder_ref))
    }

    async fn do_set_lineage(
        &self,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        // The wire rows, matched by hash: this resolver's `FoldersClient`
        // holds no label custody, so the rendered list drops every sealed set.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set lineage: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set lineage"))?;
        Ok(custody::set_lineage_by_hash(
            &reply.folders,
            &cfg,
            name_hash,
        ))
    }

    async fn do_set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        // The same two reads as `do_set_lineage`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving a set lineage: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving a set lineage"))?;
        Ok(custody::set_lineage_by_ref(
            &reply.folders,
            &cfg,
            folder_ref,
        ))
    }

    async fn do_resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        // Both reads must SUCCEED for any answer at all — a transport failure
        // is `Err` ("could not determine"), never `Ok(None)`: the old
        // `.ok()?`-to-`None` collapse read a transient roster/config failure
        // as "unbound", and every seal site downstream took that as license
        // for the owner root. (Messages carry no set name — S7.)
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving set custody: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving set custody"))?;
        resolve_custody_from(&reply.folders, &cfg, name_hash)
    }

    async fn do_resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        // The same two reads as `do_resolve`, with the same must-succeed rule.
        let reply = self
            .folders
            .list_owned_and_shared_wire()
            .await
            .map_err(|e| anyhow::anyhow!("roster read failed resolving set custody: {e}"))?;
        let cfg = self
            .custody
            .load()
            .await
            .map_err(|e| e.context("custody read failed resolving set custody"))?;
        resolve_custody_by_ref(&reply.folders, &cfg, folder_ref)
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl FolderKeyResolver for NestFolderKeyResolver<Arc<fauna_client::NestClient>> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve_ref(folder_ref).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce_ref(folder_ref).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
    }

    async fn set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        self.do_set_lineage_ref(folder_ref).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl FolderKeyResolver for NestFolderKeyResolver<fauna_rpc_wasm::WsRpcClient> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn resolve_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve_ref(folder_ref).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_nonce_ref(&self, folder_ref: &FolderRef) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce_ref(folder_ref).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
    }

    async fn set_lineage_ref(
        &self,
        folder_ref: &FolderRef,
    ) -> anyhow::Result<Option<fauna_core::folder_keys::SetNonceLineage>> {
        self.do_set_lineage_ref(folder_ref).await
    }
}

#[cfg(test)]
mod tests {
    /// The clock the stamped custody transitions take in these tests.
    const NOW: u64 = 1_000;
    use super::*;
    use fauna_core::data::FoldersConfig;
    use fauna_core::folder_keys::{FolderContentKeys, serve_custody_channel_id};
    use fauna_core::path_crypto::set_name_hash;
    use fauna_protocol::folders::FolderSummary;

    fn cfg() -> FoldersConfig {
        FoldersConfig::default()
    }

    /// Serves a configured roster to `fauna.folders.list` and counts the reads;
    /// serves `envelope` (hex, as the nest stores it) to `content_key.get`.
    #[derive(Default)]
    struct RosterNest {
        rows: std::sync::Mutex<Vec<FolderSummary>>,
        lists: std::sync::atomic::AtomicUsize,
        envelope: std::sync::Mutex<String>,
        /// Every `content_key.get` request this nest received, as sent.
        key_gets: std::sync::Mutex<Vec<fauna_protocol::folders::ContentKeyGetRequest>>,
    }

    impl RosterNest {
        fn lists(&self) -> usize {
            self.lists.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl RpcRequester for RosterNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            if kind == fauna_protocol::folders::KIND_FOLDERS_CONTENT_KEY_GET {
                let sent = fauna_protocol::encode_canonical(&payload).expect("encode request");
                self.key_gets
                    .lock()
                    .unwrap()
                    .push(fauna_protocol::decode_strict(&sent).expect("decode request"));
                let reply = fauna_protocol::encode_canonical(
                    &fauna_protocol::folders::ContentKeyGetReply {
                        sealed: self.envelope.lock().unwrap().clone(),
                        ..Default::default()
                    },
                )
                .expect("encode reply");
                return Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"));
            }
            assert_eq!(kind, fauna_protocol::folders::KIND_FOLDERS_LIST);
            self.lists.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::folders::FoldersListReply {
                    folders: self.rows.lock().unwrap().clone(),
                    extra: Default::default(),
                })
                .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The raw group id of the shared set these re-fetch tests use, and its
    /// channel.
    const GROUP: [u8; 2] = [0xD0, 0xD1];

    fn group_channel() -> [u8; 32] {
        ChannelId::from_group_id(&GROUP).0
    }

    /// The member's roster row of that set, echoing `nonce`.
    fn member_row(nonce: [u8; 32]) -> FolderSummary {
        FolderSummary {
            role: Some("member".into()),
            set_nonce: Some(nonce.to_vec().into()),
            ..row("docs", Some(hex::encode(GROUP)))
        }
    }

    /// A custody holding that set's keys and `nonce` as the member received
    /// them.
    fn member_custody(nonce: [u8; 32]) -> FoldersConfig {
        let mut c = cfg();
        let channel = group_channel();
        custody::merge_received_keys(&mut c, channel, FolderContentKeys::genesis([5; 32], 1));
        custody::record_received_set_nonce(&mut c, &channel, nonce, NOW);
        c
    }

    /// A custody whose every read fails — the own-nest link down at leave time.
    struct UnreadableStore;

    #[async_trait]
    impl FolderKeyReader for UnreadableStore {
        async fn load(&self) -> anyhow::Result<FoldersConfig> {
            Err(anyhow::anyhow!("own nest unreachable"))
        }
    }

    #[async_trait]
    impl FolderKeyStore for UnreadableStore {
        async fn merge(&self, _replica: FoldersConfig) -> anyhow::Result<FoldersConfig> {
            Err(anyhow::anyhow!("own nest unreachable"))
        }
        async fn settle_removal(
            &self,
            _removal: fauna_core::data::FolderPendingRemoval,
        ) -> anyhow::Result<FoldersConfig> {
            Err(anyhow::anyhow!("own nest unreachable"))
        }
    }

    ///: an unreadable custody is an UNKNOWN home, not a same-nest
    /// set — a leave that took `None` for it would send a same-nest leave of a
    /// foreign set (answered `left: false`) and then delete the one record a
    /// retry needs. A readable custody still answers the tri-state's other two.
    #[test]
    fn an_unreadable_custody_is_an_unknown_home_not_a_same_nest_set() {
        use fauna_client_testkit::block_on;
        let channel = group_channel();
        let nest = Arc::new(RosterNest::default());

        let unreadable = NestFolderCustodySink::new(Arc::clone(&nest), Arc::new(UnreadableStore));
        assert!(block_on(unreadable.do_foreign_home_url(&channel)).is_err());

        let readable =
            NestFolderCustodySink::new(nest, Arc::new(crate::MemoryFolderKeyStore::default()));
        assert_eq!(block_on(readable.do_foreign_home_url(&channel)), Ok(None));
    }

    /// The unknown-home rule above, one layer up: `leave_share`
    /// — the one sequence every app runs, web's `foldersLeave` included —
    /// over a backend whose RAM holds no home for the set (a relaunch, or a
    /// launch seed that could not read custody) and
    /// whose custody is unreadable stops before the roster leave goes out at
    /// all — no same-nest `folders.leave` of a possibly-foreign set, and so no
    /// `leave_folder` forget of the record a retry needs. Reddens if either the
    /// backend's tri-state read or the leave's `?` folds the unknown home back
    /// to same-nest.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_leave_with_an_unknown_home_sends_nothing() {
        use fauna_client_testkit::{FailingRequester, block_on};
        use fauna_conversations::ConversationsSession;
        use fauna_core::identity::ActorKeypair;
        use fauna_mls::engine::MlsEngine;

        let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let actor = engine.identity_actor_id();
        let session = Arc::new(ConversationsSession::from_parts(
            engine,
            Arc::new(QuietChannelNest),
            "member".into(),
            actor,
            None,
        ));
        session.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
            Arc::new(RosterNest::default()),
            Arc::new(UnreadableStore),
        )));
        let nest = FailingRequester::new("the leave went out");

        let err = block_on(crate::leave_share(
            &nest,
            &session.backend(),
            hex::encode(group_channel()),
        ))
        .expect_err("an unknown home refuses the leave");
        assert!(err.contains("custody record unreadable"), "{err}");
        assert!(nest.kinds().is_empty(), "sent {:?}", nest.kinds());
    }

    /// Answers `fauna.folders.leave` with `left: true` and keeps every request
    /// as sent; any other kind fails loud.
    #[derive(Default)]
    struct LeaveNest {
        leaves: std::sync::Mutex<Vec<fauna_protocol::folders::MemberLeaveRequest>>,
    }

    impl RpcRequester for LeaveNest {
        type Error = std::convert::Infallible;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            assert_eq!(kind, fauna_protocol::folders::KIND_FOLDERS_LEAVE);
            let sent = fauna_protocol::encode_canonical(&payload).expect("encode request");
            self.leaves
                .lock()
                .unwrap()
                .push(fauna_protocol::decode_strict(&sent).expect("decode request"));
            let reply =
                fauna_protocol::encode_canonical(&fauna_protocol::folders::MemberLeaveReply {
                    ok: true,
                    channel_id: hex::encode(group_channel()),
                    left: true,
                    extra: Default::default(),
                })
                .expect("encode reply");
            Ok(fauna_protocol::decode_strict(&reply).expect("decode reply"))
        }
    }

    /// The leave's local half on a foreign set: `leave_share` over a
    /// backend whose RAM holds no home (a relaunch) and whose readable custody
    /// records the set relays the roster leave to the RECORDED home, then
    /// tombstones the foreign-set record — so the left set stops seeding a home
    /// and stops listing as foreign. Reddens if the backend's forget drops the
    /// record drop, or if the home read stops consulting custody.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_foreign_leave_relays_home_and_tombstones_the_record() {
        use fauna_client_testkit::block_on;
        use fauna_conversations::ConversationsSession;
        use fauna_core::identity::ActorKeypair;
        use fauna_mls::engine::MlsEngine;

        const HOME: &str = "https://home.example";
        let mut c = cfg();
        custody::record_foreign_set(
            &mut c,
            fauna_core::data::ForeignFolder {
                channel_id: group_channel(),
                mls_group_id: GROUP.to_vec(),
                home_nest_url: HOME.into(),
                ..Default::default()
            },
            NOW,
        );
        let store = Arc::new(crate::MemoryFolderKeyStore::with(c));

        let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let actor = engine.identity_actor_id();
        let session = Arc::new(ConversationsSession::from_parts(
            engine,
            Arc::new(QuietChannelNest),
            "member".into(),
            actor,
            None,
        ));
        session.set_folder_custody_sink(Arc::new(NestFolderCustodySink::new(
            Arc::new(RosterNest::default()),
            Arc::clone(&store) as Arc<dyn FolderKeyStore>,
        )));
        let nest = LeaveNest::default();

        block_on(crate::leave_share(
            &nest,
            &session.backend(),
            hex::encode(GROUP),
        ))
        .expect("a known-foreign leave succeeds");

        let leaves = nest.leaves.lock().unwrap().clone();
        assert_eq!(leaves.len(), 1, "one roster leave");
        assert_eq!(
            leaves[0].nest_url.as_deref(),
            Some(HOME),
            "relayed to the home the custody records"
        );
        let after = block_on(store.load()).expect("readable custody");
        assert!(
            after
                .foreign_sets
                .iter()
                .all(|f| f.channel_id != group_channel() || !f.is_live()),
            "the left set's foreign record is tombstoned"
        );
    }

    /// A sink over `rows` and `store`, its re-fetch check throttled to
    /// `interval` microseconds.
    fn refetch_sink(
        rows: Vec<FolderSummary>,
        store: Arc<crate::MemoryFolderKeyStore>,
        interval: u64,
    ) -> (NestFolderCustodySink<Arc<RosterNest>>, Arc<RosterNest>) {
        let nest = Arc::new(RosterNest {
            rows: std::sync::Mutex::new(rows),
            ..Default::default()
        });
        let mut sink = NestFolderCustodySink::new(Arc::clone(&nest), store);
        sink.refetch_check_interval = interval;
        (sink, nest)
    }

    /// `set-addressing.md` § The row form → *Whoever holds the row sends it*:
    /// the ingest resolves a same-nest set's address from the roster by its
    /// channel and holds that row, so the `content_key.get` it sends names the
    /// row — beside a same-named own set the name forms alone address the own
    /// set and answer its envelope.
    #[test]
    fn the_same_nest_key_read_names_the_row_its_channel_resolved() {
        use fauna_client_testkit::block_on;
        let channel_hex = hex::encode(group_channel());
        let own = FolderSummary {
            id: 7,
            ..row("photos", Some(hex::encode([0xA0, 0xA1])))
        };
        let shared = FolderSummary {
            id: 9,
            role: Some("member".into()),
            ..row("photos", Some(hex::encode(GROUP)))
        };
        let store = Arc::new(crate::MemoryFolderKeyStore::default());
        let (sink, nest) = refetch_sink(vec![own, shared], store, 0);

        block_on(sink.do_fetch_sealed_envelope(&channel_hex));

        let sent = nest.key_gets.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "one key read for the channel");
        assert_eq!(
            sent[0].folder_id,
            Some(9),
            "the shared set's row, not the own namesake's"
        );
        assert!(sent[0].nest_url.is_none(), "a same-nest read");
    }

    /// `writer-signed-change-records.md` ruling (11)(b), the pushed-nonce
    /// trigger: the nest's echo of the set's nonce moving off the one the
    /// member holds owes one envelope fetch — and the check that finds it is
    /// throttled, so the quiet polls in between read no roster.
    #[test]
    fn a_moved_nonce_echo_owes_one_refetch_and_a_quiet_poll_reads_no_roster() {
        use fauna_client_testkit::block_on;
        let channel = group_channel();
        let store = Arc::new(crate::MemoryFolderKeyStore::with(member_custody(
            [0x0B; 32],
        )));

        // The echo agrees with what the member holds: nothing owed, and one
        // roster read serves every ask inside the interval.
        let (sink, nest) = refetch_sink(vec![member_row([0x0B; 32])], Arc::clone(&store), u64::MAX);
        assert!(!block_on(sink.do_refetch_owed(&channel)));
        assert!(!block_on(sink.do_refetch_owed(&channel)));
        assert_eq!(nest.lists(), 1, "a quiet poll reads no roster");

        // The owner re-minted with no epoch advance: the echo moved.
        let (sink, nest) = refetch_sink(vec![member_row([0x0C; 32])], Arc::clone(&store), u64::MAX);
        assert!(
            block_on(sink.do_refetch_owed(&channel)),
            "a pushed nonce the member does not hold owes the fetch"
        );
        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "spent by the asking — one fetch, not one per poll"
        );
        assert_eq!(nest.lists(), 1);

        // Converged: once the member's custody holds the pushed nonce, a later
        // check owes nothing.
        let (sink, _) = refetch_sink(vec![member_row([0x0C; 32])], Arc::clone(&store), 0);
        assert!(block_on(sink.do_refetch_owed(&channel)));
        block_on(update(&*store, |c| {
            custody::record_received_set_nonce(c, &channel, [0x0C; 32], NOW + 1)
        }))
        .unwrap();
        assert!(!block_on(sink.do_refetch_owed(&channel)));
    }

    /// `writer-signed-change-records.md` ruling (7)(b)(ii) rule (6)(a): an
    /// owner's re-publish that moves no epoch and no nonce — a serve flip —
    /// moves the nest's echo of the envelope's digest, and that owes the
    /// member one fetch: owed until the fetch remembers the new digest, owed
    /// nothing after. A first sighting, with nothing fetched this session,
    /// owes nothing (the launch's own fetch answers it), and a row that echoes
    /// no digest asserts nothing.
    #[test]
    fn a_moved_envelope_digest_echo_owes_one_refetch_and_a_first_sighting_owes_none() {
        use fauna_client_testkit::block_on;
        let channel = group_channel();
        let channel_hex = hex::encode(channel);
        let nonce = [0x0B; 32];
        let echoing = |blob: &[u8]| FolderSummary {
            envelope_digest: Some(
                fauna_protocol::folder_envelope_sig::digest(blob)
                    .to_vec()
                    .into(),
            ),
            ..member_row(nonce)
        };
        let store = Arc::new(crate::MemoryFolderKeyStore::with(member_custody(nonce)));
        let (sink, nest) = refetch_sink(vec![echoing(b"first")], Arc::clone(&store), 0);
        *nest.envelope.lock().unwrap() = hex::encode(b"first");

        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "a first sighting with nothing fetched owes nothing"
        );
        block_on(sink.do_fetch_sealed_envelope(&channel_hex));
        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "the echo agrees with the envelope last fetched"
        );

        // The owner re-publishes with no commit: new bytes, same epoch, same
        // nonce — only the digest echo moved.
        *nest.rows.lock().unwrap() = vec![echoing(b"second")];
        *nest.envelope.lock().unwrap() = hex::encode(b"second");
        assert!(
            block_on(sink.do_refetch_owed(&channel)),
            "a moved digest echo owes the fetch"
        );
        block_on(sink.do_fetch_sealed_envelope(&channel_hex));
        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "one fetch answers it"
        );

        // A row echoing no digest asserts nothing, whatever was fetched.
        *nest.rows.lock().unwrap() = vec![member_row(nonce)];
        assert!(!block_on(sink.do_refetch_owed(&channel)));
    }

    /// A conversations nest whose folder channel carries no commit: the member's
    /// polls are quiet. Every other method fails loud — the flow below must not
    /// reach one.
    struct QuietChannelNest;

    #[async_trait::async_trait]
    impl fauna_conversations::backend::ConversationsRpc for QuietChannelNest {
        async fn channel_send(
            &self,
            _c: String,
            _e: Vec<u8>,
            _x: Option<i64>,
            _a: Vec<String>,
        ) -> Result<i64, fauna_conversations::backend::ConvRpcError> {
            unreachable!("channel_send")
        }
        async fn channel_send_remote(
            &self,
            _c: String,
            _u: String,
            _e: Vec<u8>,
            _x: Option<i64>,
            _a: Vec<String>,
        ) -> Result<i64, fauna_conversations::backend::ConvRpcError> {
            unreachable!("channel_send_remote")
        }
        async fn channel_fetch(
            &self,
            _c: String,
            _a: i64,
            _l: i64,
            _h: Option<String>,
        ) -> Result<
            Vec<fauna_conversations::backend::FetchedRecord>,
            fauna_conversations::backend::ConvRpcError,
        > {
            Ok(vec![])
        }
        async fn keypackage_count(
            &self,
            _a: String,
        ) -> Result<u64, fauna_conversations::backend::ConvRpcError> {
            unreachable!("keypackage_count")
        }
        async fn keypackage_addressable(
            &self,
            _a: String,
        ) -> Result<bool, fauna_conversations::backend::ConvRpcError> {
            unreachable!("keypackage_addressable")
        }
        async fn actor_by_handle(
            &self,
            _h: String,
        ) -> Result<
            Option<fauna_conversations::backend::ResolvedHandle>,
            fauna_conversations::backend::ConvRpcError,
        > {
            unreachable!("actor_by_handle")
        }
        async fn actor_by_handle_remote(
            &self,
            _d: String,
            _l: String,
        ) -> Result<
            Option<fauna_conversations::backend::ResolvedHandle>,
            fauna_conversations::backend::ConvRpcError,
        > {
            unreachable!("actor_by_handle_remote")
        }
        async fn keypackage_fetch(
            &self,
            _a: String,
            _p: Option<String>,
        ) -> Result<Option<Vec<u8>>, fauna_conversations::backend::ConvRpcError> {
            unreachable!("keypackage_fetch")
        }
        async fn keypackage_upload(
            &self,
            _p: Vec<Vec<u8>>,
            _l: bool,
        ) -> Result<u64, fauna_conversations::backend::ConvRpcError> {
            unreachable!("keypackage_upload")
        }
        async fn welcome_deliver(
            &self,
            _r: String,
            _c: String,
            _w: Vec<u8>,
            _k: fauna_conversations::backend::WelcomeChannelKind,
            _p: Option<String>,
        ) -> Result<(), fauna_conversations::backend::ConvRpcError> {
            unreachable!("welcome_deliver")
        }
        async fn blob_put(
            &self,
            _c: String,
            _h: Option<String>,
            _i: Option<String>,
            _s: String,
            _b: Vec<u8>,
        ) -> Result<(), fauna_conversations::backend::ConvRpcError> {
            unreachable!("blob_put")
        }
        async fn blob_get(
            &self,
            _c: String,
            _h: Option<String>,
            _i: Option<String>,
            _s: String,
        ) -> Result<Option<Vec<u8>>, fauna_conversations::backend::ConvRpcError> {
            unreachable!("blob_get")
        }
    }

    /// The test nest's twin of the per-target trait impls above, so the
    /// member's MLS driver can run this sink's real logic.
    #[async_trait]
    impl FolderCustodySink for NestFolderCustodySink<Arc<RosterNest>> {
        async fn fetch_sealed_envelope(
            &self,
            channel_id_hex: &str,
        ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
            self.do_fetch_sealed_envelope(channel_id_hex).await
        }
        async fn merge_and_persist(
            &self,
            channel_id: &[u8; 32],
            payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
            may_move: bool,
        ) -> bool {
            self.do_merge_and_persist(channel_id, payload, may_move)
                .await
        }
        async fn record_foreign_set(&self, record: fauna_core::data::ForeignFolder) -> bool {
            self.do_record_foreign_set(record).await
        }
        async fn name_foreign_set_from_seal(
            &self,
            channel_id: &[u8; 32],
            sealed: &[u8],
            name_hash: &[u8],
        ) -> bool {
            self.do_name_foreign_set_from_seal(channel_id, sealed, name_hash)
                .await
        }
        async fn forget_foreign_set(&self, channel_id: &[u8; 32]) -> bool {
            self.do_forget_foreign_set(channel_id).await
        }
        async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Result<Option<String>, String> {
            self.do_foreign_home_url(channel_id).await
        }
        async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
            self.do_foreign_homes().await
        }
        async fn refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
            self.do_refetch_owed(channel_id).await
        }
    }

    /// `writer-signed-change-records.md` ruling (7)(b)(ii) rule (6), end to
    /// end over this sink and the member's MLS driver: an owner's serve flip
    /// re-publishes the envelope at the current epoch and posts no commit, and
    /// it still reaches the member on a quiet poll — the serve-on stamp, then
    /// serve-off's rotated generation and stamp — through the nest's digest
    /// echo alone. No commit reaches the channel, the nonce never moves, and no
    /// row is refused: before rule (6) neither re-publish reached the member.
    #[test]
    fn a_serve_flips_no_commit_re_publish_reaches_a_member_on_a_quiet_poll() {
        use fauna_client_testkit::block_on;
        use fauna_conversations::backends::fauna_mls::{
            FaunaMlsBackend, join_folder_welcome, poll_inbound_folder,
        };
        use fauna_core::folder_keys::ContentKeyEnvelopePayload;
        use fauna_core::identity::ActorKeypair;
        use fauna_mls::engine::MlsEngine;

        let owner_key = ActorKeypair::from_secret([0xA1; 32]);
        let owner = MlsEngine::new_in_memory(ActorKeypair::from_secret([0xA1; 32])).unwrap();
        let member = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let (ch, welcome) = owner
            .create_group(&member.generate_key_packages(1).unwrap())
            .unwrap();
        let raw_group = owner.group_id_bytes(&ch).unwrap();
        let nonce = [0x0B; 32];

        let store = Arc::new(crate::MemoryFolderKeyStore::with(cfg()));
        let (sink, nest) = refetch_sink(Vec::new(), Arc::clone(&store), 0);
        // The owner's `content_key.put`: the nest stores the signed blob and
        // its list row echoes the blob's digest — nothing else moves.
        let publish = |payload: &ContentKeyEnvelopePayload| {
            let sealed = owner
                .seal_content_key_envelope_payload(&ch, payload)
                .unwrap()
                .sealed;
            let blob = fauna_protocol::folder_envelope_sig::sign(&owner_key, &ch.0, &sealed);
            *nest.envelope.lock().unwrap() = hex::encode(&blob);
            *nest.rows.lock().unwrap() = vec![FolderSummary {
                role: Some("member".into()),
                set_nonce: Some(nonce.to_vec().into()),
                envelope_digest: Some(
                    fauna_protocol::folder_envelope_sig::digest(&blob)
                        .to_vec()
                        .into(),
                ),
                ..row("docs", Some(hex::encode(&raw_group)))
            }];
        };
        let held = || block_on(store.load()).unwrap();

        let gen1 = FolderContentKeys::genesis([1; 32], 1_000);
        let mut unserved = ContentKeyEnvelopePayload {
            keys: gen1.clone(),
            set_nonce: Some(nonce),
            minted_by: None,
            retired_set_nonces: Vec::new(),
            served_at: None,
            unserved_at: None,
        };
        publish(&unserved);
        let backend = FaunaMlsBackend::new(
            Arc::clone(&member),
            Arc::new(QuietChannelNest),
            "member",
            member.identity_actor_id(),
        );
        backend.set_folder_custody_sink(Arc::new(sink));
        block_on(join_folder_welcome(
            &backend,
            &ch.to_string(),
            &welcome.to_bytes().unwrap(),
            "",
            &fauna_conversations::session::FolderWelcomeContext::default(),
        ))
        .expect("the member joins the shared set");
        assert!(
            custody::content_keys(&held(), &ch.0).is_some(),
            "the join ingest"
        );
        assert!(!custody::channel_served(&held(), &ch.0));

        // Serve-on: the same keys at the same epoch, now stamped served.
        let served = ContentKeyEnvelopePayload {
            served_at: Some(2_000),
            ..unserved.clone()
        };
        publish(&served);
        let mut cursor = 0i64;
        let outcome = block_on(poll_inbound_folder(&backend, &ch, &mut cursor, 0)).unwrap();
        assert_eq!(outcome.applied, 0, "no commit reached the channel");
        assert!(
            custody::channel_served(&held(), &ch.0),
            "the serve-on stamp reached the member with no commit"
        );

        // Serve-off: the owner rotates and re-publishes, again with no commit.
        let mut gen2 = gen1;
        gen2.rotate([2; 32], 3_000);
        unserved = ContentKeyEnvelopePayload {
            keys: gen2,
            unserved_at: Some(3_000),
            ..served
        };
        publish(&unserved);
        block_on(poll_inbound_folder(&backend, &ch, &mut cursor, 0)).unwrap();
        let after = held();
        assert!(
            custody::content_keys(&after, &ch.0).is_some_and(|k| k.key_for(2).is_some()),
            "the rotated generation reached the member"
        );
        assert!(
            !custody::channel_served(&after, &ch.0),
            "and the serve-off stamp"
        );
    }

    /// A row the caller OWNS is never the member trigger's: the owner's nonce
    /// is custody's own, and its echo lags its own re-mint by construction.
    #[test]
    fn an_owned_rows_echo_owes_nothing() {
        use fauna_client_testkit::block_on;
        let store = Arc::new(crate::MemoryFolderKeyStore::with(member_custody(
            [0x0B; 32],
        )));
        let owned = FolderSummary {
            role: Some("owner".into()),
            ..member_row([0x0C; 32])
        };
        let (sink, _) = refetch_sink(vec![owned], store, 0);
        assert!(!block_on(sink.do_refetch_owed(&group_channel())));
    }

    /// Ruling (11)(b), the `signature_invalid` trigger: the engine host's
    /// request owes one fetch per burst — however many refusals stamped it
    /// between two checks — and a request older than this session owes none
    /// (the launch fetched). A foreign set, which has no roster row to echo a
    /// nonce, is reached the same way.
    #[test]
    fn an_engine_hosts_request_owes_one_refetch_per_burst() {
        use fauna_client_testkit::block_on;
        let channel = group_channel();
        let mut c = member_custody([0x0B; 32]);
        let foreign = foreign_record("away");
        custody::record_foreign_set(&mut c, foreign.clone(), NOW);
        let store = Arc::new(crate::MemoryFolderKeyStore::with(c));
        block_on(store.request_refetch(&channel, 10)).unwrap();
        let (sink, _) = refetch_sink(vec![member_row([0x0B; 32])], Arc::clone(&store), 0);

        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "a request from before this session was answered by the launch's fetch"
        );
        assert!(!block_on(sink.do_refetch_owed(&foreign.channel_id)));

        // A burst of refusals: the host stamps twice before the next check.
        block_on(store.request_refetch(&channel, 20)).unwrap();
        block_on(store.request_refetch(&channel, 30)).unwrap();
        assert!(block_on(sink.do_refetch_owed(&channel)));
        assert!(
            !block_on(sink.do_refetch_owed(&channel)),
            "one fetch per burst — the same stamp owes nothing twice"
        );

        block_on(store.request_refetch(&foreign.channel_id, 40)).unwrap();
        assert!(
            block_on(sink.do_refetch_owed(&foreign.channel_id)),
            "a foreign set has no echo; the request is what reaches it"
        );
        assert!(!block_on(sink.do_refetch_owed(&channel)));
    }

    fn row(name: &str, group_hex: Option<String>) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: name.into(),
            mls_group_id: group_hex,
            ..Default::default()
        }
    }

    /// A roster row flagged for WebDAV serving (`folders.webdav_enabled`).
    fn served_row(name: &str, group_hex: Option<String>) -> FolderSummary {
        FolderSummary {
            webdav_enabled: true,
            ..row(name, group_hex)
        }
    }

    /// The content-keyed payload, or a panic naming the arm that answered.
    fn content_keyed(got: ResolvedCustody) -> ResolvedFolderKeys {
        match got {
            ResolvedCustody::ContentKeyed(keys) => keys,
            ResolvedCustody::OwnerOnly { .. } => panic!("expected a content-keyed answer"),
        }
    }

    fn is_plain_owner_only(got: &ResolvedCustody) -> bool {
        matches!(
            got,
            ResolvedCustody::OwnerOnly {
                retired_content_keys: None
            }
        )
    }

    /// A foreign (cross-nest) record whose display name is chosen by someone
    /// ELSE — the collision ingredient.
    fn foreign_record(name: &str) -> fauna_core::data::ForeignFolder {
        fauna_core::data::ForeignFolder {
            channel_id: [0xAA; 32],
            mls_group_id: vec![0xAA, 0xAA, 0xAA],
            home_nest_url: "https://stranger.example".into(),
            home_nest_actor_id: None,
            set_name: Some(name.into()),
            access: None,
            content_key_floor: None,
            ..Default::default()
        }
    }

    /// A record's nonce resolves by the same name resolution as its custody:
    /// an owned unbound set by the owner's live pick, a member row by its
    /// channel — never borrowing the caller's own same-named set — and a
    /// foreign record by its channel.
    #[test]
    fn the_set_nonce_resolves_by_the_same_name_resolution_as_custody() {
        let mut c = cfg();
        custody::record_created_set(&mut c, "docs", [0x01; 32], None, 10);
        let owned = row("docs", None);
        assert_eq!(
            resolve_set_nonce_from(std::slice::from_ref(&owned), &c, "docs"),
            Some([0x01; 32])
        );

        // A member row of someone else's "docs": its channel's received nonce.
        let raw = vec![0xB0, 0xB1];
        let channel = ChannelId::from_group_id(&raw).0;
        custody::merge_received_keys(&mut c, channel, FolderContentKeys::genesis([5; 32], 1));
        custody::record_received_set_nonce(&mut c, &channel, [0x0B; 32], NOW);
        let member = FolderSummary {
            role: Some("member".into()),
            ..row("docs", Some(hex::encode(&raw)))
        };
        assert_eq!(
            resolve_set_nonce_from(std::slice::from_ref(&member), &c, "docs"),
            Some([0x0B; 32])
        );

        // A foreign record, no roster row: by its channel.
        let foreign = foreign_record("away");
        c.foreign_sets.push(foreign.clone());
        custody::merge_received_keys(
            &mut c,
            foreign.channel_id,
            FolderContentKeys::genesis([6; 32], 1),
        );
        custody::record_received_set_nonce(&mut c, &foreign.channel_id, [0x0F; 32], NOW);
        assert_eq!(resolve_set_nonce_from(&[], &c, "away"), Some([0x0F; 32]));
        assert_eq!(resolve_set_nonce_from(&[], &c, "nowhere"), None);
    }

    /// A record that names its row binds the nonce of THAT row, never the one
    /// a name lookup picks: a writer member who also owns a set of the same
    /// name signs an upload into the shared one under the shared set's nonce —
    /// the one the nest verifies the row under (`set-addressing.md` § The row
    /// form; `writer-signed-change-records.md` ruling (2)).
    #[test]
    fn the_set_nonce_by_row_tells_a_same_named_pair_apart() {
        let mut c = cfg();
        custody::record_created_set(&mut c, "docs", [0x01; 32], None, 10);
        let own = FolderSummary {
            id: 7,
            ..row("docs", None)
        };
        let raw = vec![0xB0, 0xB1];
        let channel = ChannelId::from_group_id(&raw).0;
        custody::merge_received_keys(&mut c, channel, FolderContentKeys::genesis([5; 32], 1));
        custody::record_received_set_nonce(&mut c, &channel, [0x0B; 32], NOW);
        let shared = FolderSummary {
            id: 9,
            role: Some("member".into()),
            ..row("docs", Some(hex::encode(&raw)))
        };
        let roster = vec![own, shared];
        assert_eq!(
            resolve_set_nonce_by_ref(&roster, &c, &FolderRef::Local(9)),
            Some([0x0B; 32]),
            "the shared row's nonce"
        );
        assert_eq!(
            resolve_set_nonce_by_ref(&roster, &c, &FolderRef::Local(7)),
            Some([0x01; 32]),
            "the own row's nonce"
        );
        assert_eq!(
            resolve_set_nonce_by_ref(&roster, &c, &FolderRef::Local(8)),
            None
        );

        // The reader's half: the lineage a projected row is judged under. By
        // hash the pair answers the OWN set's; by row, each its own.
        let lineage_at =
            |id| custody::set_lineage_by_ref(&roster, &c, &FolderRef::Local(id)).map(|l| l.live);
        assert_eq!(
            custody::set_lineage_by_hash(&roster, &c, &set_name_hash("docs")).live,
            Some([0x01; 32])
        );
        assert_eq!(
            lineage_at(9),
            Some(Some([0x0B; 32])),
            "the shared row's lineage"
        );
        assert_eq!(
            lineage_at(7),
            Some(Some([0x01; 32])),
            "the own row's lineage"
        );
        assert_eq!(lineage_at(8), None, "no such row: no by-row answer");
    }

    /// The probe, inverted into a pin: an owned row named `photos`
    /// exists and is UNBOUND, and a foreign record of the same freely-chosen
    /// name — with resolvable keys — sits in the folder-keys custody. The answer is
    /// `Ok(None)`: positively unbound, owner-key custody.
    #[test]
    fn an_unbound_owned_rows_custody_is_the_owner_root_never_a_colliding_foreign_sets() {
        let mut c = cfg();
        custody::record_foreign_set(&mut c, foreign_record("photos"), NOW);
        custody::merge_received_keys(
            &mut c,
            [0xAA; 32],
            FolderContentKeys::genesis([0x42; 32], 1_000),
        );

        let got =
            resolve_custody_from(&[row("photos", None)], &c, &set_name_hash("photos")).unwrap();

        assert!(
            is_plain_owner_only(&got),
            "an unbound owned set's seal root is the owner root — a colliding \
             foreign record must never resolve it to a stranger's roster key"
        );
    }

    /// A served, unshared set is content-keyed at the serve pseudo-channel
    /// (`webdav-server.md` § Key model, the custody note) — the read-side twin
    /// of `EngineKeyBinding::ServedUnshared`. Its keys resolve with NO group
    /// id: a served set never fakes one to reach the content-key path.
    #[test]
    fn a_served_unshared_row_resolves_content_keyed_at_the_serve_pseudo_channel() {
        let mut c = cfg();
        custody::record_new_set(
            &mut c,
            serve_custody_channel_id("photos"),
            [0x42; 32],
            1_000,
        );
        // Keyed and stamp-less is NOT served, whatever the roster flags
        // (ruling (7)(b)(ii) rule (2)): the owner-only answer, the keys as
        // retired read candidates.
        assert!(matches!(
            resolve_custody_from(&[served_row("photos", None)], &c, &set_name_hash("photos"))
                .unwrap(),
            ResolvedCustody::OwnerOnly {
                retired_content_keys: Some(_)
            }
        ));
        custody::serve_on(&mut c, &serve_custody_channel_id("photos"), 2_000);

        let got = content_keyed(
            resolve_custody_from(&[served_row("photos", None)], &c, &set_name_hash("photos"))
                .unwrap(),
        );

        assert!(got.mls_group_id.is_none(), "served-unshared: no group id");
        assert_eq!(
            got.content_keys.expect("serve custody").current_version(),
            1
        );
        assert!(got.home_nest_url.is_none());
    }

    /// Served by custody's word but the generations have not joined this
    /// device's folder-keys custody yet: content-keyed-but-
    /// unresolvable, never owner-only — `ServedKeysMissing`'s read-side twin,
    /// so the seal sites fail closed.
    #[test]
    fn a_served_row_with_no_serve_custody_yet_is_content_keyed_but_unresolvable() {
        let mut c = cfg();
        c.sets.push(fauna_core::data::FolderKeyCustody {
            channel_id: Some(serve_custody_channel_id("photos")),
            served_at: Some(2_000),
            ..Default::default()
        });
        let got = content_keyed(
            resolve_custody_from(&[served_row("photos", None)], &c, &set_name_hash("photos"))
                .unwrap(),
        );

        assert!(got.mls_group_id.is_none());
        assert!(
            got.content_keys.is_none(),
            "served-but-keyless: seal sites must fail closed on this shape"
        );
    }

    /// A set both shared and served keys its custody at the REAL channel
    /// (`custody_channel_for`: the group wins; serving migrated the
    /// pseudo-channel custody there at bind time) — the group id rides.
    #[test]
    fn a_served_and_shared_row_resolves_by_its_real_channel() {
        let raw = b"raw-group-id".to_vec();
        let channel = ChannelId::from_group_id(&raw).0;
        let mut c = cfg();
        custody::merge_received_keys(
            &mut c,
            channel,
            FolderContentKeys::genesis([0x42; 32], 1_000),
        );

        let got = content_keyed(
            resolve_custody_from(
                &[served_row("docs", Some(hex::encode(&raw)))],
                &c,
                &set_name_hash("docs"),
            )
            .unwrap(),
        );

        assert_eq!(got.mls_group_id.as_deref(), Some(raw.as_slice()));
        assert_eq!(
            got.content_keys.expect("group custody").current_version(),
            1
        );
    }

    /// A served-then-unflagged set is owner-only again — but the generations
    /// its served window sealed under are handed back as RETIRED read
    /// candidates (`retired_serve_custody`), so the owner still renders and
    /// opens the served-era files and names (`webdav-server.md` § Key model,
    /// Revocation).
    #[test]
    fn an_unflagged_row_with_serve_history_hands_back_retired_generations() {
        let mut c = cfg();
        let serve = serve_custody_channel_id("photos");
        custody::record_new_set(&mut c, serve, [0x42; 32], 1_000);
        // Unflagging rotates rather than forgets (`serve_disable`).
        custody::rotate_set(&mut c, &serve, [0x43; 32], 2_000);

        let got =
            resolve_custody_from(&[row("photos", None)], &c, &set_name_hash("photos")).unwrap();

        let ResolvedCustody::OwnerOnly {
            retired_content_keys,
        } = got
        else {
            panic!("an unflagged set is owner-only again");
        };
        let retired = retired_content_keys.expect("the served window's generations ride");
        assert_eq!(retired.current_version(), 2);
        assert_eq!(retired.keys_for(1).count(), 1, "gen 1 stays openable");
    }

    /// The same two answers for a SEALED set, whose roster row rests no
    /// plaintext name since schema 114 (`path-sealing.md` § the set-name
    /// plane). The serve pseudo-channel is derived from the set's NAME, so the
    /// row is named from custody by hash before any channel is derived: a
    /// blank name resolved the channel of the empty string, which read a
    /// served set as plain owner-only and an unserved one as never served —
    /// its served-era labels then omitted from the owner's own Media page.
    #[test]
    fn a_scrubbed_rows_serve_custody_resolves_under_the_name_custody_holds() {
        let mut c = cfg();
        custody::record_created_set(&mut c, "photos", [0x11; 32], None, 500);
        let serve = serve_custody_channel_id("photos");
        custody::key_named_set(&mut c, "photos", serve, [0x42; 32], 1_000);
        custody::serve_on(&mut c, &serve, 2_000);
        let scrubbed = FolderSummary {
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                set_name_hash("photos").to_vec(),
            )),
            ..row("", None)
        };

        let served = content_keyed(
            resolve_custody_from(
                std::slice::from_ref(&scrubbed),
                &c,
                &set_name_hash("photos"),
            )
            .unwrap(),
        );
        assert_eq!(
            served
                .content_keys
                .expect("serve custody")
                .current_version(),
            1
        );

        custody::rotate_set(&mut c, &serve, [0x43; 32], 3_000);
        custody::serve_off(&mut c, &serve, 4_000);
        let ResolvedCustody::OwnerOnly {
            retired_content_keys,
        } = resolve_custody_from(&[scrubbed], &c, &set_name_hash("photos")).unwrap()
        else {
            panic!("an unserved set is owner-only again");
        };
        let retired = retired_content_keys.expect("the served window's generations ride");
        assert_eq!(retired.keys_for(1).count(), 1, "gen 1 stays openable");
    }

    /// The resolver half: a bound row whose content keys this
    /// custody cannot produce keeps its bound-ness (`mls_group_id` rides,
    /// `content_keys: None`) instead of collapsing to "unbound".
    #[test]
    fn a_bound_row_with_absent_custody_keeps_its_bound_ness() {
        let got = content_keyed(
            resolve_custody_from(
                &[row("docs", Some(hex::encode(b"raw-group-id")))],
                &cfg(),
                &set_name_hash("docs"),
            )
            .unwrap(),
        );

        assert_eq!(got.mls_group_id.as_deref(), Some(&b"raw-group-id"[..]));
        assert!(
            got.content_keys.is_none(),
            "bound-but-unresolvable: seal sites must fail closed on this shape"
        );
        assert!(got.home_nest_url.is_none());
    }

    /// A row the nest projects with its plaintext `name` scrubbed — a sealed
    /// set once `folders.name` is contracted — still resolves, by the
    /// `name_hash` it carries: its custody (keys) and its set nonce alike. Keyed
    /// by the plaintext, both would answer for `""` — owner-only and no nonce
    /// — and a bound set's sealed name would render under the wrong root.
    #[test]
    fn a_scrubbed_row_resolves_by_its_name_hash_alone() {
        let raw = vec![0xC0, 0xC1];
        let channel = ChannelId::from_group_id(&raw).0;
        let mut c = cfg();
        custody::merge_received_keys(&mut c, channel, FolderContentKeys::genesis([7; 32], 1));
        custody::record_received_set_nonce(&mut c, &channel, [0x0C; 32], NOW);
        let scrubbed = FolderSummary {
            name_hash: Some(set_name_hash("docs").to_vec().into()),
            role: Some("member".into()),
            ..row("", Some(hex::encode(&raw)))
        };

        let got = content_keyed(
            resolve_custody_from(std::slice::from_ref(&scrubbed), &c, &set_name_hash("docs"))
                .unwrap(),
        );
        assert_eq!(got.mls_group_id.as_deref(), Some(&raw[..]));
        assert!(
            got.content_keys.is_some(),
            "the member's received generations"
        );
        assert_eq!(
            resolve_set_nonce_from(std::slice::from_ref(&scrubbed), &c, "docs"),
            Some([0x0C; 32])
        );
        assert!(
            is_plain_owner_only(
                &resolve_custody_from(std::slice::from_ref(&scrubbed), &c, &set_name_hash(""))
                    .unwrap()
            ),
            "the blank plaintext addresses nothing"
        );
    }

    /// The ordinary bound read: held custody resolves with its keys.
    #[test]
    fn a_bound_row_with_held_custody_resolves_its_keys() {
        let raw = b"raw-group-id".to_vec();
        let channel = ChannelId::from_group_id(&raw).0;
        let mut c = cfg();
        custody::merge_received_keys(
            &mut c,
            channel,
            FolderContentKeys::genesis([0x42; 32], 1_000),
        );

        let got = content_keyed(
            resolve_custody_from(
                &[row("docs", Some(hex::encode(&raw)))],
                &c,
                &set_name_hash("docs"),
            )
            .unwrap(),
        );

        assert_eq!(got.mls_group_id.as_deref(), Some(raw.as_slice()));
        assert_eq!(got.content_keys.expect("keys held").current_version(), 1);
    }

    /// Corrupt bound-row state is an ERROR, not "unbound" — `Ok(None)`
    /// licenses the owner root at every seal site.
    #[test]
    fn a_bound_rows_undecodable_group_hex_is_an_error_not_unbound() {
        let got = resolve_custody_from(
            &[row("docs", Some("zz-not-hex".into()))],
            &cfg(),
            &set_name_hash("docs"),
        );
        assert!(got.is_err(), "corrupt bound state must fail closed as Err");
    }

    /// Do-not-cheat control (resolver half): a
    /// genuinely foreign name — no owned row — still resolves foreign, with
    /// its home-nest byte route.
    #[test]
    fn a_genuinely_foreign_name_resolves_foreign_with_its_home_url() {
        let mut c = cfg();
        custody::record_foreign_set(&mut c, foreign_record("their-docs"), NOW);
        custody::merge_received_keys(
            &mut c,
            [0xAA; 32],
            FolderContentKeys::genesis([0x42; 32], 1_000),
        );

        let got = content_keyed(
            resolve_custody_from(&[row("my-docs", None)], &c, &set_name_hash("their-docs"))
                .unwrap(),
        );

        assert_eq!(got.mls_group_id, Some(vec![0xAA, 0xAA, 0xAA]));
        assert_eq!(
            got.home_nest_url.as_deref(),
            Some("https://stranger.example")
        );
        assert!(got.content_keys.is_some());
    }

    /// No row anywhere and no foreign record: positively unbound.
    #[test]
    fn no_row_and_no_foreign_record_is_positively_unbound() {
        let got = resolve_custody_from(&[], &cfg(), &set_name_hash("absent")).unwrap();
        assert!(is_plain_owner_only(&got));
    }

    /// A bound owned/member row also shadows a colliding foreign record — the
    /// roster answer wins whenever a row exists, bound or not.
    #[test]
    fn a_bound_owned_row_shadows_a_colliding_foreign_record() {
        let mut c = cfg();
        custody::record_foreign_set(&mut c, foreign_record("photos"), NOW);

        let got = content_keyed(
            resolve_custody_from(
                &[row("photos", Some(hex::encode(b"raw-group-id")))],
                &c,
                &set_name_hash("photos"),
            )
            .unwrap(),
        );

        assert_eq!(
            got.mls_group_id.as_deref(),
            Some(&b"raw-group-id"[..]),
            "the OWN group, not the stranger's"
        );
        assert!(got.home_nest_url.is_none(), "and no foreign byte route");
    }

    /// A reader's own UNBOUND `photos` (id 7) listed before a member row of
    /// someone else's `photos` (id 9) whose group keys custody holds.
    fn shadowed_member_roster() -> (Vec<FolderSummary>, FoldersConfig) {
        let own = FolderSummary {
            id: 7,
            ..row("photos", None)
        };
        let member = FolderSummary {
            id: 9,
            role: Some("member".into()),
            ..row("photos", Some(hex::encode(GROUP)))
        };
        let mut c = cfg();
        custody::merge_received_keys(
            &mut c,
            group_channel(),
            FolderContentKeys::genesis([5; 32], 1),
        );
        (vec![own, member], c)
    }

    /// The by-identity resolution (`mls-group-key-material.md` § M2 →
    /// *Resolving custody — by the set's identity, never by its name*): the
    /// member row the reader's own same-named set shadows BY NAME resolves by
    /// its id to its group's keys, while the by-name resolution still answers
    /// the shadowing owner-only set.
    #[test]
    fn a_local_ref_resolves_the_row_with_that_id_past_a_same_named_own_set() {
        let (roster, c) = shadowed_member_roster();

        let got = content_keyed(resolve_custody_by_ref(&roster, &c, &FolderRef::Local(9)).unwrap());
        assert_eq!(got.mls_group_id.as_deref(), Some(&GROUP[..]));
        assert_eq!(
            got.content_keys,
            custody::content_keys(&c, &group_channel()),
            "the member row's own group keys"
        );
        assert!(got.content_keys.is_some());
        assert!(got.home_nest_url.is_none());

        // The by-name shadow the doc declares: the owner-first pick.
        assert!(is_plain_owner_only(
            &resolve_custody_from(&roster, &c, &set_name_hash("photos")).unwrap()
        ));
        // And the own row by ITS id answers what the name does.
        assert!(is_plain_owner_only(
            &resolve_custody_by_ref(&roster, &c, &FolderRef::Local(7)).unwrap()
        ));
    }

    /// A local ref naming no roster row is "could not determine", never the
    /// positive owner-only answer a by-name miss gives.
    #[test]
    fn a_local_ref_naming_no_row_is_an_error_not_owner_only() {
        let (roster, c) = shadowed_member_roster();
        assert!(resolve_custody_by_ref(&roster, &c, &FolderRef::Local(11)).is_err());
    }

    /// A sealed own row (its name scrubbed) resolves by id through the same
    /// custody naming the by-name pick applies: its serve custody, at the
    /// pseudo-channel of the name custody holds.
    #[test]
    fn a_sealed_own_rows_ref_resolves_under_the_name_custody_holds() {
        let mut c = cfg();
        custody::record_created_set(&mut c, "photos", [0x11; 32], None, 500);
        let serve = serve_custody_channel_id("photos");
        custody::key_named_set(&mut c, "photos", serve, [0x42; 32], 1_000);
        custody::serve_on(&mut c, &serve, 2_000);
        let sealed = FolderSummary {
            id: 4,
            name: String::new(),
            name_hash: Some(fauna_protocol::ByteBuf::from(
                set_name_hash("photos").to_vec(),
            )),
            ..row("", None)
        };

        let got =
            content_keyed(resolve_custody_by_ref(&[sealed], &c, &FolderRef::Local(4)).unwrap());
        assert_eq!(got.content_keys, custody::content_keys(&c, &serve));
        assert!(got.content_keys.is_some(), "the serve custody");
        assert!(got.mls_group_id.is_none(), "served-unshared: no group");
    }

    /// A foreign ref resolves the live record at that channel — home route
    /// included, whatever its name — and a channel with no live record is an
    /// error.
    #[test]
    fn a_foreign_ref_resolves_the_record_at_that_channel_and_no_other() {
        let (roster, mut c) = shadowed_member_roster();
        // Named `photos` too: the by-name pick can never reach it.
        custody::record_foreign_set(&mut c, foreign_record("photos"), NOW);
        custody::merge_received_keys(
            &mut c,
            [0xAA; 32],
            FolderContentKeys::genesis([0x42; 32], 1_000),
        );

        let got = content_keyed(
            resolve_custody_by_ref(&roster, &c, &FolderRef::Foreign([0xAA; 32])).unwrap(),
        );
        assert_eq!(got.mls_group_id, Some(vec![0xAA, 0xAA, 0xAA]));
        assert_eq!(
            got.home_nest_url.as_deref(),
            Some("https://stranger.example")
        );
        assert!(got.content_keys.is_some());

        assert!(resolve_custody_by_ref(&roster, &c, &FolderRef::Foreign([0xBB; 32])).is_err());
    }
}
