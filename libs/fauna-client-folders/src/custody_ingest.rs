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
use fauna_conversations::backend::FolderCustodySink;
use fauna_core::folder_keys::{FolderKeyResolver, ResolvedCustody, ResolvedFolderKeys};
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
/// nudge the agent re-resolves on (`on-demand-files.md` § Shared sets on a
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
    /// - **a row was refused `signature_invalid`**: the engine host's request
    ///   stamp for the channel ([`FolderKeyReader::request_refetch`]) moved
    ///   since the last check that saw it. This is what reaches a foreign
    ///   set, which has no roster row here to echo anything.
    ///
    /// Best-effort like every sink read: an unreadable custody finds nothing,
    /// an unreadable roster leaves the echo arm for the next check.
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
    /// `(name, name_hash)` pair `content_key.get` takes. The member-visible
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
    ) -> Option<(String, Option<fauna_protocol::ByteBuf>)> {
        let reply = self.folders.list_owned_and_shared_wire().await.ok()?;
        reply.folders.into_iter().find_map(|s| {
            let group_hex = s.mls_group_id.as_ref()?;
            let raw = hex::decode(group_hex.trim()).ok()?;
            (ChannelId::from_group_id(&raw).to_string() == channel_id_hex)
                .then_some((s.name, s.name_hash))
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
        let request = if let Some((name, name_hash)) = self.resolve_address(channel_id_hex).await {
            ContentKeyGetRequest {
                name,
                name_hash,
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
        // hold arms from it — `on-demand-files.md` § Shared sets on a capability
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
            // it — a pair or nothing (`federation.md` § … *The cross-nest
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

    async fn do_foreign_home_url(&self, channel_id: &[u8; 32]) -> Option<String> {
        let cfg = self.custody.load().await.ok()?;
        custody::find_foreign_set(&cfg, channel_id).map(|f| f.home_nest_url.clone())
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

    async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Option<String> {
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

    async fn foreign_home_url(&self, channel_id: &[u8; 32]) -> Option<String> {
        self.do_foreign_home_url(channel_id).await
    }

    async fn foreign_homes(&self) -> Vec<([u8; 32], String)> {
        self.do_foreign_homes().await
    }

    async fn refetch_owed(&self, channel_id: &[u8; 32]) -> bool {
        self.do_refetch_owed(channel_id).await
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
/// by this name until the collision dissolves. Addressing both is the
/// channel-keyed custody direction (menu (c)), an S9-era change.
pub(crate) fn resolve_custody_from(
    roster: &[fauna_protocol::folders::FolderSummary],
    cfg: &fauna_core::data::FoldersConfig,
    name_hash: &[u8; 32],
) -> anyhow::Result<ResolvedCustody> {
    if let Some(row) = custody::find_named_roster_row(roster, cfg, name_hash) {
        let row = &row;
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
        return Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
            content_keys: custody::content_keys(cfg, &channel),
            mls_group_id,
            home_nest_url: None,
            home_nest_actor_id: None,
        }));
    }
    // No roster row — a FOREIGN (cross-nest) set resolves from the member's
    // own custody record instead (written at accept time; Phase 2 client
    // read-side): identity + the home-nest base URL the byte fetch must use. A
    // name-less foreign record (an unopenable seal) is unaddressable by
    // name and stays owner-only — its custody-ingest reads still work by
    // channel.
    let Some(foreign) = custody::find_foreign_set_by_hash(cfg, name_hash) else {
        return Ok(ResolvedCustody::owner_only());
    };
    Ok(ResolvedCustody::ContentKeyed(ResolvedFolderKeys {
        mls_group_id: Some(foreign.mls_group_id.clone()),
        content_keys: custody::content_keys(cfg, &foreign.channel_id),
        home_nest_url: Some(foreign.home_nest_url.clone()),
        // The grant-delivered trust root for the byte-plane dial
        // (`refresh_foreign_set_from_reply` keeps it current from every
        // federated read reply).
        home_nest_actor_id: foreign.home_nest_actor_id.clone(),
    }))
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
}

#[cfg(not(target_arch = "wasm32"))]
#[async_trait]
impl FolderKeyResolver for NestFolderKeyResolver<Arc<fauna_client::NestClient>> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl FolderKeyResolver for NestFolderKeyResolver<fauna_rpc_wasm::WsRpcClient> {
    async fn resolve(&self, name_hash: &[u8; 32]) -> anyhow::Result<ResolvedCustody> {
        self.do_resolve(name_hash).await
    }

    async fn set_nonce(&self, folder: &str) -> anyhow::Result<Option<[u8; 32]>> {
        self.do_set_nonce(folder).await
    }

    async fn set_lineage(
        &self,
        _folder: &str,
        name_hash: &[u8; 32],
    ) -> anyhow::Result<fauna_core::folder_keys::SetNonceLineage> {
        self.do_set_lineage(name_hash).await
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

    /// Serves a configured roster to `fauna.folders.list` and counts the reads.
    #[derive(Default)]
    struct RosterNest {
        rows: std::sync::Mutex<Vec<FolderSummary>>,
        lists: std::sync::atomic::AtomicUsize,
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
            _payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
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
}
