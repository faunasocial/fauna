//! WASM wrappers for ConversationsManager.
//!
//! The manager is kept **transport-free** (same split linux uses between its
//! `ConversationsManager` and `apps/fauna-linux/src/conversations/mail_sink.rs`):
//! it owns parse + bucket + ingest, while the browser SPA drives the
//! `fauna.email.inbox.fetch` poll in JS and hands each fetched record to
//! [`WasmConversationsManager::ingest_sealed_inbound`]. The browser owns the
//! timer + lifecycle, so the loop lives in JS rather than the shared
//! `poll_inbound_mail` driver. (The `InboundMailSource` seam is now dual-armed
//! `?Send` per Track E2a, so a wasm source over
//! the `Rc`-based WS-RPC client is also possible — routing web through
//! `poll_inbound_mail` directly is a viable future refactor, not blocking.)
//! `docs/goal/ui/conversations.md` §
//! Receiving into the conversations view.

use async_trait::async_trait;
use fauna_client_mail_settings::InboxSpamScorer;
use fauna_client_moderation::LocalDetectionStore;
use fauna_conversations::backend::{
    InboundMailRecord, MailFeed, OutboundMailSink, RoomSeams, SelfAddress,
};
use fauna_conversations::backends::fauna_mls::FaunaMlsBackend;
use fauna_conversations::backends::smtp::{
    SmtpBackend, WantedMailRecord, ingest_inbound_record, refill_mail_record_attachments,
    take_wanted_mail_records,
};
use fauna_conversations::room_settings::{RoomSettingsDraft, RoomSettingsEdit};
use fauna_conversations::{
    ConversationsManager, MessageId, MessageNotificationTracker, SortOrder, ThreadActivity,
    ThreadId, TypedAddress, try_parse_typed_address,
};
use fauna_core::identity::ActorId;
use fauna_mail::spam::BayesianKnobs;
use fauna_mls::types::ChannelId;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

// The FaunaMls rail's construction + inbound/wire drivers are wasm-only: they
// reach the `WsConversationsRpc` seam + `MlsEngine` over the `Rc`-based WS-RPC
// client, and the async drivers bridge to JS via `future_to_promise`.
#[cfg(target_arch = "wasm32")]
use fauna_client_drafts::DraftsSync;
// Cross-device MLS group-state sync (devices.md § Cross-device MLS group-state
// sync): the `WsMlsReplicaTransport` adapter (over `WsRpcClient`) + the shared
// `MlsStateSync` wrapper and the `FaunaCommitGate`/`MlsSyncCursor` device-owned-epoch
// plane — the web twin of linux `conv_backend.rs`'s `wire_mls_state_sync`.
#[cfg(target_arch = "wasm32")]
use fauna_client_conversations::WsMlsReplicaTransport;
#[cfg(target_arch = "wasm32")]
use fauna_client_mls_sync::MlsStateSync;
#[cfg(target_arch = "wasm32")]
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backends::fauna_mls::{ingest_welcome, poll_inbound_conv_past_key_in};
#[cfg(target_arch = "wasm32")]
use fauna_core::identity::ActorKeypair;
#[cfg(target_arch = "wasm32")]
use fauna_mls::engine::MlsEngine;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::future_to_promise;
// Owner-side shared-folder content-key orchestration, driven by the folder
// author methods below (they reuse this manager's WS-RPC transport + the
// conversations rail's `MlsEngine`). Wasm-only — `share_set` reaches the real
// `Arc<MlsEngine>` `FolderGroupCrypto` adapter (`fauna-client-folders/mls`).
#[cfg(target_arch = "wasm32")]
// The unattested-member review's config-store seam (`decide_member_review` /
// `load_member_reviews`) + the shared row-text/verdict types — the wasm twin
// of fauna-ffi's `member_review.rs`.
#[cfg(target_arch = "wasm32")]
use fauna_client_config::{decide_member_review, load_member_reviews};
// The Nostr succession-aftermath npub-confirm banner's shared read
// (`npub_confirmation_owed_for`), the stamp itself on the account plane
// through the tab's runtime handle — the wasm twin of fauna-ffi's
// `nostr_npub_confirm.rs`.
#[cfg(target_arch = "wasm32")]
use fauna_client_config::DavStoreContext;
#[cfg(target_arch = "wasm32")]
use fauna_client_config::npub_confirmation_owed_for;
#[cfg(target_arch = "wasm32")]
use fauna_client_conversations::ConversationsClient;
#[cfg(target_arch = "wasm32")]
use fauna_core::data::{MemberReview, MemberUnattestedReason, UnattestedVerdict, review_row_text};
// The shared `channel_type` → seam-kind decoder (single vocabulary source, also
// used by the native drain) + the shared recipient contact gate the web welcome
// drain hands to `ingest_welcome_by_kind` (`folders.md` § Sharing — the gate is
// one implementation for all 7 apps, priority #2).
#[cfg(target_arch = "wasm32")]
use fauna_client_conversations::{NestFolderGate, wire_channel_type_to_kind};
#[cfg(target_arch = "wasm32")]
use fauna_client_folders::FoldersClient;
#[cfg(target_arch = "wasm32")]
use fauna_client_folders::orchestration::FoldersAuthor;
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backend::FolderGateSink;
// The single Welcome-ingest dispatch, shared with the native receive rail, plus
// the two recipient-side leaves the pending-share faces drive.
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backends::fauna_mls::{join_folder_welcome, leave_folder};
#[cfg(target_arch = "wasm32")]
use fauna_conversations::session::{
    FolderWelcomeContext, ingest_welcome_by_kind, poll_folder_feed, poll_scheduling_feed,
};
// The crypto-free calendar-apply seam the scheduling drain hands each decrypted
// iMIP to — the browser registers `WebSchedulingSink` on it, exactly as the
// native apps register `NestSchedulingSink`.
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backend::SchedulingSink;

#[cfg(target_arch = "wasm32")]
use fauna_client_inbox::PENDING_SHARE_PEEK_LIMIT;
// The bridged rail: the shared backend and receive driver, over the shared
// glue's seams (`fauna_client_conversations::NestBridgedGlue`).
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backend::BridgedSource;
#[cfg(target_arch = "wasm32")]
use fauna_conversations::backends::bridged::{BridgedBackend, poll_inbound_bridged};

/// The browser's key source for the bridged rail's glue — the wasm twin of the
/// native `MailKeyCache`. It reads the cells the JS receive loop fills
/// ([`WasmConversationsManager::set_recipient_keypairs`] and its two
/// siblings), so the glue opens under exactly the key set the SMTP rail opens
/// mail with. `None` until all of them are set — mail not enabled.
#[cfg(target_arch = "wasm32")]
struct WebBridgedKeySource {
    standing: Rc<RefCell<Vec<fauna_mls::wrapped_blob::StandingMailKeypair>>>,
    epoch_roots: Rc<RefCell<Vec<[u8; 32]>>>,
    own_public: Rc<RefCell<Option<fauna_mls::wrapped_blob::XWingPublicKey>>>,
}

/// One read of [`WebBridgedKeySource`]'s cells, owned across the glue's awaits.
#[cfg(target_arch = "wasm32")]
struct WebBridgedKeys {
    standing: Vec<fauna_mls::wrapped_blob::StandingMailKeypair>,
    epoch_roots: Vec<[u8; 32]>,
    own_public: fauna_mls::wrapped_blob::XWingPublicKey,
}

#[cfg(target_arch = "wasm32")]
impl fauna_client_conversations::BridgedKeys for WebBridgedKeys {
    fn seal_for_self(&self, body: &str) -> Result<Vec<u8>, String> {
        fauna_client_conversations::seal_bridged_for_self(&self.own_public, body)
    }

    fn open_bridged(&self, sealed: &[u8], received_at_ms: i64) -> Option<String> {
        // No retirement instants cross the JS seam yet: the whole set walks
        // newest first (every generation still opens).
        fauna_client_conversations::open_bridged_row(
            &self.standing,
            &self.epoch_roots,
            &[],
            sealed,
            received_at_ms,
        )
    }
}

#[cfg(target_arch = "wasm32")]
impl fauna_client_conversations::BridgedKeySource for WebBridgedKeySource {
    type Keys = WebBridgedKeys;

    async fn get(&self) -> Option<WebBridgedKeys> {
        let standing = self.standing.borrow().clone();
        let own_public = self.own_public.borrow().clone()?;
        if standing.is_empty() {
            return None;
        }
        Some(WebBridgedKeys {
            standing,
            epoch_roots: self.epoch_roots.borrow().clone(),
            own_public,
        })
    }

    /// The cells are the JS loop's to refill, so a re-read is all there is to
    /// do here.
    async fn refresh(&self) -> Option<WebBridgedKeys> {
        self.get().await
    }
}

/// The browser `ConversationsRpc` seam impl (over the WS-RPC `WsRpcClient`) the
/// web SPA registers on its `FaunaMlsBackend` — the Track E web unblock
/// (the native twin is `NestConversationsRpc`). Re-exported here so the
/// forthcoming wasm backend-construction glue (build the MLS engine, register
/// the backend, drive `poll_inbound_conv` + the welcome push — mirroring
/// `apps/fauna-linux/src/conversations/conv_backend.rs`) can reach it.
#[cfg(target_arch = "wasm32")]
pub use fauna_client_conversations::WsConversationsRpc;
// The shared outbound-mail send sink (the send twin of `NestMailInboundSource`) —
// the browser registers it over `WsRpcClient`, exactly as the native apps do
// over `Arc<NestClient>`. Replaces the former wasm-local `WasmSmtpSink` copy.
#[cfg(target_arch = "wasm32")]
use fauna_client_conversations::NestOutboundMailSink;

/// The no-send SMTP sink for the client-less [`WasmConversationsManager::new`]
/// path. Registering an `SmtpBackend` (even with this no-op sink) is what makes
/// `ConversationsManager::ingest_inbound` resolve the `Rail::Smtp` backend and
/// bucket inbound mail onto a thread — so a manager built without a WS-RPC
/// client can still *receive*. A `submit` call means a code path tried to send
/// through a receive-only manager; surface it loudly rather than dropping.
/// The send-capable path is `WasmConversationsManager::with_conversations`, which
/// wires a real `NestOutboundMailSink` over `EmailClient<WsRpcClient>`.
struct ReceiveOnlySink;

// Match the seam's dual-armed shape (`-3` Track E2a): `Send` futures off wasm,
// `?Send` on wasm (the `Rc`-based WS-RPC world). A plain `#[async_trait]` here
// would emit a `Send` future that mismatches the wasm `?Send` trait signature.
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl OutboundMailSink for ReceiveOnlySink {
    async fn submit(&self, _recipients: Vec<String>, _raw_rfc5322: Vec<u8>) -> Result<(), String> {
        Err("outbound mail send not wired: build the manager via WsRpcClient.conversationsManager(selfAddress)".to_string())
    }
}

/// The browser's calendar-apply sink for the **mailbox-less CalDAV iMIP rail** —
/// the wasm twin of the native `fauna_client_conversations::NestSchedulingSink`,
/// registered on the same crypto-free [`SchedulingSink`] seam and routing to the
/// same shared [`CalDavClient::apply_inbound_scheduling_from_message`] (REQUEST →
/// materialize, CANCEL → tombstone, REPLY → merge an RSVP). Only the transport
/// differs: `WsRpcClient` here, `Arc<NestClient>` there (priority #2 — the whole
/// apply, including the parse and the seal, stays in shared Rust and the MSEK
/// never crosses into JS).
///
/// Unlike the native sink this holds its keys **eagerly**, because the browser
/// derives them once per drain in [`WasmConversationsManager::poll_scheduling`]
/// rather than caching a mail-custody load for the life of a process: a tab's
/// receive tick already owns the `secret_hex` for that call, and a sink that
/// outlived it would be holding key material the manager deliberately does not
/// retain.
#[cfg(target_arch = "wasm32")]
struct WebSchedulingSink {
    client: fauna_rpc_wasm::WsRpcClient,
    /// The actor and its MSEK history: a write seals to the current
    /// generation, a lookup of the stored event walks the whole ring.
    ctx: DavStoreContext,
    /// The write's epoch-seconds CREATED/LAST-MODIFIED surrogate, from JS (the
    /// wasm-time discipline — no `Date::now()` in wasm).
    now_secs: i64,
    /// Where a refused change is recorded (`inbound-scheduling-authority.md`
    /// § *Surfacing*) — the manager's refused-change inbox, which reaches the
    /// account plane's `fauna.state.refused-scheduling-changes` row through
    /// the seam web's runtime registers at its store-ready edge, exactly as
    /// the native sink's does.
    refused: Arc<fauna_conversations::refused_changes::RefusedChangeInbox>,
    /// Answers the inbound rule's questions that need the network: who an
    /// address names, and whether a bound organizer verifiably succeeded to the
    /// message's author — the latter at most once per identity per session.
    resolver: fauna_client_caldav::MemoizedSuccessionResolver<
        fauna_client_caldav::DiscoveryPrincipalResolver<fauna_client_caldav::AnonAttendeeDiscovery>,
        WebOrganizerSuccessionDialer,
    >,
}

/// The browser's `INBOX` read-state door — the two `\Seen` calls the shared
/// mail read-state sync makes ([`WasmConversationsManager::sync_mail_read_state`]),
/// through the same call mapping the native `INBOX` source uses
/// (`fauna_client_conversations::{inbox_mark_seen_call, inbox_flag_changes_call}`).
/// The mailbox itself is still drained by the JS loop, so `fetch` is never
/// asked and answers an empty page.
#[cfg(target_arch = "wasm32")]
struct WebInboxReadState {
    email: fauna_client_email::EmailClient<fauna_rpc_wasm::WsRpcClient>,
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl fauna_conversations::backend::InboundMailSource for WebInboxReadState {
    async fn fetch(
        &self,
        _after_uid: u32,
        _limit: u32,
    ) -> Result<fauna_conversations::backend::InboundMailPage, String> {
        Ok(Default::default())
    }

    async fn mark_seen(
        &self,
        uids: Vec<u32>,
    ) -> Result<(), fauna_conversations::MailFlagCallError> {
        fauna_client_conversations::inbox_mark_seen_call(&self.email, uids).await
    }

    async fn flag_changes(
        &self,
        since_modseq: u64,
        after_uid: u32,
        limit: u32,
    ) -> Result<fauna_conversations::MailFlagChangesPage, fauna_conversations::MailFlagCallError>
    {
        fauna_client_conversations::inbox_flag_changes_call(
            &self.email,
            since_modseq,
            after_uid,
            limit,
        )
        .await
    }
}

/// The browser's succession dial for the inbound-mutation rule — the wasm twin
/// of the native sink's, handed to the same shared
/// [`MemoizedSuccessionResolver`](fauna_client_caldav::MemoizedSuccessionResolver)
/// (`caldav-server.md` § Who may mutate an existing event over the inbound
/// rail → *A succeeded organizer*). The memo, the anchor and the
/// once-per-session rule are the shared resolver's; addresses go to the shared
/// anon `by_handle` discovery beside it, whose dial is the browser's own
/// `AnonymousWsRpcClient`. The rule names the nest to dial — the one the event
/// was bound to, blank = this tab's own — and the two kinds the walk needs are
/// pre-identity, so the anonymous dial web's succession witness already makes
/// covers it.
#[cfg(target_arch = "wasm32")]
struct WebOrganizerSuccessionDialer {
    /// The manager whose peer-anchor store holds the account's held chain
    /// heads (`fauna.state.peer-anchors`) — read only when a succession is
    /// actually asked about. `Weak`, like every other holder of it.
    manager: std::sync::Weak<fauna_conversations::ConversationsManager>,
}

#[cfg(target_arch = "wasm32")]
impl fauna_client_caldav::SuccessionDialer for WebOrganizerSuccessionDialer {
    async fn walk(
        &self,
        old_actor_id: &str,
        anchor_nest_url: &str,
    ) -> fauna_client_caldav::SuccessionLookup {
        use fauna_client_caldav::SuccessionLookup;
        let Ok(old) = fauna_core::identity::ActorId::from_hex(old_actor_id) else {
            return SuccessionLookup::NotAsked;
        };
        // Unreadable anchors — the account store not lent yet, or a failed
        // read — leave "do we hold a head?" unknown, and the honest answer to
        // unknown is *no answer* — never a walk quietly downgraded to
        // first-contact grade. No dial was made, so the shared memo does not
        // remember it.
        let Some(store) = self.manager.upgrade().and_then(|m| m.peer_anchor_store()) else {
            return SuccessionLookup::NotAsked;
        };
        let Ok(anchors) = store.peer_anchors().await else {
            return SuccessionLookup::NotAsked;
        };
        let known_head = anchors.known_chain_head(&old);
        use fauna_client_recovery::AnchoredWalk;
        match crate::succession_witness::walk_at_nest_url(anchor_nest_url, old, known_head).await {
            AnchoredWalk::Succeeded(step) => {
                SuccessionLookup::Succeeded(step.new_actor_id.to_hex())
            }
            AnchoredWalk::NeverSucceeded => SuccessionLookup::NotSucceeded,
            AnchoredWalk::Unsettled => SuccessionLookup::Unproven,
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[async_trait(?Send)]
impl SchedulingSink for WebSchedulingSink {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        origin: fauna_conversations::backend::SchedulingOrigin,
    ) -> Result<(), String> {
        // Read the METHOD off the same extract the apply routes on, so the
        // refused-change row can never disagree with it about what arrived.
        let method = fauna_client_caldav::scheduling_method(&raw_rfc5322);
        let inbound_origin = fauna_client_caldav::InboundOrigin {
            author: origin.author,
            home_nest_url: origin.home_nest_url,
        };
        let outcome = fauna_client_caldav::CalDavClient::new(self.client.clone())
            .apply_inbound_scheduling_from_message(
                &self.ctx.actor_id,
                &self.ctx.msek,
                &self.ctx.prior_mseks,
                &raw_rfc5322,
                self.now_secs,
                // Pass-through only — who may create / change / cancel is
                // decided once, in the shared apply (`caldav-server.md` § Who
                // may mutate an existing event over the inbound rail). The
                // browser's resolver names no address, so a bound event
                // compares locally and an unbound one is refused; what it does
                // answer is a bound organizer's verified succession.
                &inbound_origin,
                &self.resolver,
            )
            .await
            .map_err(|e| e.to_string())?;
        // The native sink's twin: a refusal is someone trying to change this
        // calendar and not being allowed to — a `warn`; an apply is rare (one
        // per delivered invitation), so a diagnosis breadcrumb, not tick noise.
        if outcome.is_refused() {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "refused inbound scheduling iMIP: {outcome:?}"
            )));
            // ... and the user hears about it on the Events page, through the
            // same shared record the native sink writes. The inbox never
            // blocks and never fails the drain: the calendar is already
            // untouched.
            if let Some(row) =
                outcome.refused_change_record(&method, &inbound_origin, self.now_secs)
            {
                self.refused.record(row);
            }
        } else {
            web_sys::console::debug_1(&JsValue::from_str(&format!(
                "applied inbound scheduling iMIP: {outcome:?}"
            )));
        }
        Ok(())
    }
}

#[wasm_bindgen]
pub struct WasmConversationsManager {
    manager: Arc<ConversationsManager>,
    /// The account's complete **standing** recipient-mail key set — the current
    /// MSEK's keypair (X25519 secret + ML-KEM-768 decaps half) first, then one
    /// per prior grace generation — set once via [`Self::set_recipient_keypairs`]
    /// when mail is seen enabled; the nest only ever holds the public halves.
    /// The raw MSEK never reaches here: the mail-settings machine derives the
    /// set in wasm (`WasmMailSettingsMachine::recipientStandingKeypairs`) from
    /// the ONE shared derivation the MDA's snapshot is built from. `[0]` is also
    /// the key the spam model and Nostr DM content are sealed to. Empty ⇒ mail
    /// not enabled.
    ///
    /// `Rc`, like the two cells below it: the bridged rail's glue reads the
    /// same three through its key source ([`WebBridgedKeySource`]).
    standing_keypairs: Rc<RefCell<Vec<fauna_mls::wrapped_blob::StandingMailKeypair>>>,
    /// The account's own X-Wing recipient **public** key — what the bridged
    /// rail seals the user's own copy of a sent message to — set via
    /// [`Self::set_recipient_public_key`] beside the key set. `None` ⇒ mail
    /// not enabled.
    recipient_public: Rc<RefCell<Option<fauna_mls::wrapped_blob::XWingPublicKey>>>,
    /// The client's mail-epoch roots for the content-sealing-epochs opener chain
    /// (design § 4/§ 5) — current MSEK's root then one per prior grace generation,
    /// set via [`Self::set_mail_epoch_roots`] from
    /// `WasmMailSettingsMachine::mailEpochRoots` alongside the key set.
    /// Empty until mail is enabled (standing-sealed mail is still written when the
    /// append clock read fails and `stored_at` rests 0); `ingest_sealed_inbound`
    /// passes them to the epoch opener, which falls back to the standing
    /// recipient secret so standing-sealed mail still opens.
    mail_epoch_roots: Rc<RefCell<Vec<[u8; 32]>>>,
    /// The bridged rail's receive half — the one [`BridgedBackend`] registered
    /// on the manager and the glue's source seam, as
    /// `ConversationsSession::register_bridged` keeps them natively. `None` on
    /// the client-less constructor.
    #[cfg(target_arch = "wasm32")]
    bridged: RefCell<Option<(Arc<BridgedBackend>, Arc<dyn BridgedSource>)>>,
    /// The bridged inbox's row cursor and dedup set, for
    /// [`Self::poll_bridged`]. `Option` inside the cell is the re-entrancy
    /// guard [`Self::peer_anchor_sweep`] uses: a pass takes it across its
    /// awaits, and an overlapping one finds it gone and skips.
    #[cfg(target_arch = "wasm32")]
    bridged_cursor: Rc<RefCell<Option<(i64, HashSet<i64>)>>>,
    /// Dedup by server segment-record id across overlapping re-polls
    /// (`ingest_inbound` is not idempotent — a record must not be ingested twice).
    seen: RefCell<HashSet<Vec<u8>>>,
    /// The new-message OS-banner decision, stateful across snapshot ticks —
    /// the SAME `MessageNotificationTracker` linux holds as a Rust dep and the
    /// three native apps hold over UniFFI (`conversations.md` § Where logic
    /// lives: the when/for-whom decision is shared, only the *firing* is app
    /// glue). Owned by the manager rather than by the SPA so its lifetime is
    /// the session's: `resetConversationsManager` (an actor switch) builds a
    /// fresh one, and the incoming identity's restored threads then seed
    /// silently instead of raising a banner apiece.
    notif_tracker: MessageNotificationTracker,
    /// The on-device INBOX spam scorer for this actor, active only while mail
    /// scoring is enabled — a *trained* model was fetched + unwrapped via
    /// [`Self::enable_spam_scoring`]. `None` on cold start (no model) / mail
    /// disabled / auto-Junk off, in which case [`Self::ingest_sealed_inbound`]
    /// scores nothing (mirrors the MDA's early return). Fed each just-decrypted
    /// INBOX message at ingest; drained by [`Self::take_spam_disposition`] at the
    /// end of a drain pass to issue one `apply_spam_disposition`
    /// (`mail-spam.md` § Scoring placement — the Fauna-app position). Held here
    /// so the decrypted body + the unwrapped model never cross into JS.
    spam_scorer: RefCell<Option<InboxSpamScorer>>,
    /// This session's post-decrypt moderation [`LocalDetectionStore`] — the wasm
    /// twin of the handle `ConversationsSession::from_manager` installs natively
    /// (`libs/fauna-conversations/src/session.rs`). The shared receive-loop writer
    /// (`ConversationsManager::observe_local_detection`) already runs on the web
    /// path but no-ops while no store is installed, so this handle is what makes
    /// the moderation queue's **local half** non-empty: it retains the spam
    /// detections classified from each just-decrypted incoming message — the only
    /// social-content signal in encrypted mode, where the nest holds ciphertext and
    /// classifies nothing (`moderation.md` § Layout & flow + § State & data shape).
    /// Session-scoped by design: held client-side, never round-tripped through the
    /// nest. `Arc<Mutex<_>>` because that is the handle shape the shared manager
    /// takes; on wasm (single-threaded) the lock never contends.
    local_detections: Arc<Mutex<LocalDetectionStore>>,
    /// The live self-address cell shared by both rail backends
    /// (`conversations.md` § State & data shape → *Self-address: live, never
    /// baked*): the web twin of `ConversationsSession::self_address`. Seeded by
    /// the constructor (possibly empty — construction never waits for identity
    /// resolution) and updated via [`Self::set_self_address`], which the SPA
    /// calls from its identity-store subscription so a late-resolving or
    /// renamed handle heals the cached manager without a rebuild.
    self_address: Arc<SelfAddress>,
    /// The registered FaunaMls rail backend, kept so the JS-owned inbound loop can
    /// drive the channel poll + welcome-join free functions (which take a concrete
    /// `&FaunaMlsBackend`, not a `dyn RailBackend`). `None` on the receive-only /
    /// SMTP-only constructors (`new`); `Some` once [`Self::with_conversations`]
    /// wires the MLS engine. The type compiles off-wasm (it lives in
    /// `fauna-conversations`), so the field is target-uniform; it is only ever
    /// populated + driven on wasm.
    fauna_mls: RefCell<Option<Arc<FaunaMlsBackend>>>,
    /// Per-channel paging cursor (highest server `seq` seen) for the FaunaMls
    /// inbound poll — the wasm twin of linux `conv_backend.rs`'s `cursors`. The
    /// channel log is monotonic, so the cursor alone dedups. `Rc` so the
    /// `pollConversations` future can borrow it across the `await`-free read/write
    /// around each per-channel fetch.
    conv_cursors: Rc<RefCell<HashMap<ChannelId, i64>>>,
    /// The [`Self::poll_folders`] twin of [`Self::conv_cursors`] — per-**folder**
    /// channel commit cursors, kept separate because folder channels are
    /// commit-only, bind no thread, and are derived from the engine rather than
    /// from `bound_channels()` (the native `ConversationsSession::folder_cursors`).
    folder_cursors: Rc<RefCell<HashMap<ChannelId, i64>>>,
    /// The [`Self::poll_scheduling`] twin of [`Self::folder_cursors`] — per-channel
    /// paging cursors for the **scheduling** (mailbox-less CalDAV iMIP) rail, kept
    /// separate for the same reason: a scheduling channel is a one-off delivery
    /// group that binds no thread, so it is outside `bound_channels()` too. The
    /// native twin is `ConversationsSession::scheduling_cursors`.
    scheduling_cursors: Rc<RefCell<HashMap<ChannelId, i64>>>,
    /// The scheduling rail's remembered succession answers — one dial per bound
    /// organizer identity per session (`caldav-server.md` § Who may mutate an
    /// existing event over the inbound rail → *A succeeded organizer*). Kept
    /// here, beside [`Self::scheduling_cursors`], because the resolver is built
    /// per [`Self::poll_scheduling`] tick; the native twin is
    /// `NestSchedulingSink`'s own memo.
    scheduling_successions: fauna_client_caldav::SuccessionMemo,
    /// The in-group succession witness this seat registered on its backend,
    /// kept as the concrete type because the state contract must call
    /// `ChainWitness::observation()`, which the `dyn SuccessionWitness` the
    /// backend holds cannot answer. `None` on the receive-only / SMTP-only
    /// constructors, and on a `with_conversations` whose secret would not
    /// parse — which degrades a received succession to the bare add rather
    /// than to a guard that only *looks* present. The native twins are tui's
    /// `ConversationsState::succession_witness` and fauna-ffi's
    /// `SuccessionReportHolder`.
    #[cfg(target_arch = "wasm32")]
    succession_witness: RefCell<Option<Arc<crate::succession_witness::WebChainWitness>>>,
    /// The peer-anchor harvest's own per-peer report — the producer half of
    /// `data.succession_witness`, which the witness above cannot see (a peer
    /// that was never harvested and one that was harvested and refused both
    /// read as "no anchor" from the consumer's side).
    #[cfg(target_arch = "wasm32")]
    peer_anchor_harvest: Arc<fauna_client_recovery::harvest::HarvestLog>,
    /// The harvest sweep's cross-pass memory — the once-per-peer-per-session
    /// guard and the retry ladder, shared policy driven from this app's own
    /// clock (`succession_witness.rs`, reason 2).
    ///
    /// `Option` inside the cell is a **re-entrancy guard, not laziness**: the
    /// pass borrows it mutably across awaits, and two overlapping
    /// `pollConversations` promises would otherwise double-borrow and panic.
    /// A tick that finds the state taken simply skips its sweep — the next
    /// tick runs it, and a skipped sweep costs at most one tick of anchoring
    /// delay.
    #[cfg(target_arch = "wasm32")]
    peer_anchor_sweep: Rc<RefCell<Option<fauna_client_recovery::harvest::PeerAnchorSweepState>>>,
    /// A clone of the WS-RPC client, retained so the JS-owned receive loop can
    /// build an `InboxClient` and drive the shared durable inbox `drain`
    /// (`drainInbox`) — the web FaunaMls receive backstop (`api-layers.md` §
    /// Inbox & Messaging layer 4). `None` on the receive-only / SMTP-only `new`
    /// constructor (no client); `Some` once [`Self::with_conversations`] wires the
    /// real backend. Wasm-only (the `WsRpcClient` type is `Rc`-based + wasm-gated).
    #[cfg(target_arch = "wasm32")]
    nest_client: RefCell<Option<fauna_rpc_wasm::WsRpcClient>>,
    /// Owns this actor's conversations-rail [`DraftsSync`] — the shared launch
    /// gate + last-saved baseline over `DraftsClient` (`fauna.drafts.{get,put}`,
    /// sealed under the owner's `BackupKey`; `file-sync.md` § Drafts Sync), built
    /// once in [`Self::with_conversations`] from the same transport + identity.
    /// `None` on the client-less `new` constructor. `restoreDrafts` (launch) runs
    /// `DraftsSync::load` → `restore_drafts`; `saveDrafts` (debounced compose
    /// change) runs `DraftsSync::save_if_changed(drafts_snapshot_bytes())`. The
    /// gate ALSO carries the no-data-loss safety: a save is a no-op until a launch
    /// load *succeeds*, so a failed (incl. undecryptable) restore can never let a
    /// later save clobber the user's real nest-stored drafts — the shared wrapper
    /// subsumes the web-local `drafts_blocked` latch this leg used to keep.
    /// `Rc` so the `future_to_promise` async bodies own a clone. Wasm-only (the
    /// `WsRpcClient` is `Rc`-based + wasm-gated).
    #[cfg(target_arch = "wasm32")]
    drafts_sync: Option<Rc<DraftsSync<fauna_rpc_wasm::WsRpcClient>>>,
    /// The cross-device MLS state-sync wrapper (`devices.md` § Cross-device MLS
    /// group-state sync) — the openMLS `provider` snapshot + per-channel
    /// `history/<ch>` slices synced under the owner's `BackupKey` through the
    /// `__mls` reserved folder (`fauna.mls.{get,put}`). Built once in
    /// [`Self::with_conversations`] over [`WsMlsReplicaTransport`], `None` on the
    /// client-less `new` path. `restoreMlsState` (launch) runs `sync.load()` +
    /// restore + injects the [`FaunaCommitGate`]/[`MlsSyncCursor`] device-owned-epoch
    /// plane; `saveMlsState` (debounced) snapshots + seals + uploads. The launch
    /// gate carries the same no-data-loss safety as drafts: a save is a no-op until
    /// a launch load *succeeds*, so a failed restore can never clobber the user's
    /// real nest-stored replica. `Arc` (not `Rc`) because [`FaunaCommitGate::new`]
    /// and [`MlsSyncCursor::new`] take `Arc<MlsStateSync>`. Wasm-only.
    #[cfg(target_arch = "wasm32")]
    mls_sync: Option<Arc<MlsStateSync>>,
}

// Wasm-only like its one caller (`feed::manager`, the browser feed manager).
#[cfg(target_arch = "wasm32")]
impl WasmConversationsManager {
    /// The manager and its FaunaMls backend — what the account runtime's
    /// store-ready edge registers the account-plane seams on
    /// (`crate::account_runtime`; `conversation_seams::wire_parts`). `None` on
    /// a manager built without the FaunaMls rail.
    pub(crate) fn account_seam_parts(&self) -> Option<crate::account_runtime::SeamParts> {
        let backend = self.fauna_mls.borrow().clone()?;
        Some((Arc::clone(&self.manager), backend))
    }

    /// The room-post seam over this manager and its FaunaMls backend — what
    /// `WasmFeedManager::setRoomPostKeys` installs (`ui/feed.md` § Encryption
    /// at rest → *Room-restricted — the app half*). `None` on a manager built
    /// without the FaunaMls rail, which holds no room's keys. The same
    /// [`fauna_conversations::RoomPostSeam`] a native session answers through.
    pub(crate) fn room_post_seam(&self) -> Option<fauna_conversations::RoomPostSeam> {
        let backend = self.fauna_mls.borrow().clone()?;
        Some(fauna_conversations::RoomPostSeam::new(
            Arc::clone(&self.manager),
            backend,
        ))
    }
}

/// Parse a 64-hex actor id into an [`ActorId`] — the boundary helper for the
/// typed-Fauna chip injectors (`acceptAddParticipantFaunaChip`,
/// `removeParticipant`), mirroring linux `conv_backend.rs::parse_actor_hex`.
fn parse_actor_hex(actor_id_hex: &str) -> Result<ActorId, JsValue> {
    let bytes = hex::decode(actor_id_hex.trim())
        .map_err(|e| JsValue::from_str(&format!("actor_id not hex: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| JsValue::from_str("actor_id must be 32 bytes"))?;
    Ok(ActorId(arr))
}

/// Derive an [`ActorKeypair`] from a 64-hex 32-byte owner secret — the folder
/// author seam's secret-arg boundary (the hex twin of the FFI `Vec<u8>` secret;
/// the secret never lands in JS-visible state). Mirrors the rpc.rs `secret_hex`
/// convention.
#[cfg(target_arch = "wasm32")]
fn keypair_from_secret_hex(secret_hex: &str) -> Result<ActorKeypair, JsValue> {
    let bytes = hex::decode(secret_hex.trim())
        .map_err(|e| JsValue::from_str(&format!("owner secret not hex: {e}")))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| JsValue::from_str("owner secret must be 32 bytes"))?;
    Ok(ActorKeypair::from_secret(arr))
}

/// Decode a 64-hex derived `ChannelId` (the folder's custody/roster address).
#[cfg(target_arch = "wasm32")]
fn parse_channel_hex(channel_hex: &str) -> Result<[u8; 32], JsValue> {
    let bytes = hex::decode(channel_hex.trim())
        .map_err(|e| JsValue::from_str(&format!("channel id not hex: {e}")))?;
    bytes
        .try_into()
        .map_err(|_| JsValue::from_str("channel id must be 32 bytes"))
}

/// The account's attested predecessor ids
/// (`AccountRegistry::attested_predecessor_actor_ids`) for `actor_hex` — what
/// the served-set walk judges a retired identity's head with, in place of a
/// succession lookup (writer-signed change records, ruling (8)(b) source
/// (ii)). Empty for an identity that never succeeded.
fn attested_predecessor_ids(actor_hex: &str) -> Vec<[u8; 32]> {
    crate::succession::account_registry()
        .attested_predecessor_actor_ids(actor_hex)
        .into_iter()
        .map(|id| id.0)
        .collect()
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Whether this tab runs the served-blob follower now
    /// (`foldersResumePendingRemovals` starts it; it clears when the
    /// follower ends with its runtime, so the next sign-in starts another).
    static SERVED_BLOB_FOLLOWING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Unwrap the folder author's two reused parts from this manager's RefCells: the
/// WS-RPC transport (`nest_client`) + the conversations rail's per-actor
/// [`MlsEngine`] (the same `Arc` the chat groups use). `Err` if the manager was
/// built receive-only (no `with_conversations`), so the JS caller sees a clear
/// rejection rather than a panic.
#[cfg(target_arch = "wasm32")]
fn folders_author_parts(
    client: Option<fauna_rpc_wasm::WsRpcClient>,
    backend: Option<Arc<FaunaMlsBackend>>,
) -> Result<
    (
        fauna_rpc_wasm::WsRpcClient,
        Arc<MlsEngine>,
        Arc<FaunaMlsBackend>,
    ),
    JsValue,
> {
    let client = client.ok_or_else(|| {
        JsValue::from_str(
            "folders: conversations not wired (build via WsRpcClient.conversationsManager)",
        )
    })?;
    let backend = backend
        .ok_or_else(|| JsValue::from_str("folders: conversations not wired (no MLS engine)"))?;
    let engine = backend.engine();
    // The backend doubles as the author's FolderCommitGate: with the
    // multi-device plane wired (`set_commit_gate`) a member removal rides the
    // device-owned-epoch rebase loop (Rule 1, devices.md § Cross-device MLS
    // group-state sync); with no gate it reports `NoGate` and the ungated
    // staged discipline runs.
    Ok((client, engine, backend))
}

/// JS shape of `FoldersAuthor::share_set`'s `ShareOutcome` (`channelId` hex +
/// `inboxId`). The web twin of fauna-ffi's `FfiShareOutcome`.
#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShareOutcomeJs {
    channel_id: String,
    inbox_id: i64,
}

/// JS shape of one staged, not-yet-accepted cross-user folder share the
/// recipient renders as a `folder-pending-share` — the web twin of fauna-ffi's
/// `FfiPendingShare` (`libs/fauna-ffi/src/folders_recipient.rs`), carrying only
/// display + action metadata. The raw Welcome bytes deliberately stay in Rust:
/// `foldersAcceptShare` re-resolves them by `inboxId`, so the large blob never
/// crosses into JS.
#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PendingShareJs {
    inbox_id: i64,
    shared_by: Option<String>,
    shared_by_handle: Option<String>,
    /// The cross-nest sharer's handle domain (`None` same-nest).
    shared_by_domain: Option<String>,
    /// The pre-computed "Shared by ‹…›" label — render verbatim, never re-derive
    /// (`fauna_core::format::account_display_label` over `qualified_handle`, folded in
    /// `fauna-client-inbox`). Empty only for a fully unstamped share ⇒ the client
    /// renders its unknown-sharer i18n label.
    shared_by_display: String,
    group_id: Option<String>,
    channel_id: Option<String>,
    set_name: Option<String>,
}

/// JS shape of `FoldersAuthor::remove_member`'s `RemoveOutcome` (`commit` hex|null
/// + `evicted` + `rotated`). The web twin of fauna-ffi's `FfiRemoveOutcome`.
#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoveOutcomeJs {
    commit: Option<String>,
    evicted: bool,
    rotated: bool,
}

/// JS shape of one open unattested-member review item (`memberReviewList`) —
/// `person` as hex (this crate's actor-id convention), `reasons` as wire
/// strings via [`MemberUnattestedReason::as_wire`]. The web twin of fauna-ffi's
/// `FfiMemberReview`.
#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberReviewJs {
    person: String,
    reasons: Vec<String>,
}

#[cfg(target_arch = "wasm32")]
impl From<MemberReview> for MemberReviewJs {
    fn from(r: MemberReview) -> Self {
        Self {
            person: hex::encode(r.person.0),
            reasons: r.reasons.iter().map(|x| x.as_wire().to_string()).collect(),
        }
    }
}

/// JS shape of a scored drain pass's outcome (`takeSpamDisposition`): the two UID
/// lists the JS receive loop hands to `applySpamDisposition` after the INBOX
/// drain. Ungated (plain `u32` lists — no wasm-only types), serialized via the
/// same json-compatible serializer as `snapshot()`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SpamDispositionJs {
    scored_uids: Vec<u32>,
    junk_uids: Vec<u32>,
}

/// Build this session's moderation [`LocalDetectionStore`] and hand the manager its
/// handle — the wasm twin of what `ConversationsSession::from_manager` does for the
/// native apps (`libs/fauna-conversations/src/session.rs`). **Both** constructors
/// call it, so every web manager classifies post-decrypt: the shared writer's
/// "no store installed" no-op arm is unreachable on the web path
/// (`moderation.md` § Layout & flow — the queue's local half).
fn install_local_detection_store(
    manager: &Arc<ConversationsManager>,
) -> Arc<Mutex<LocalDetectionStore>> {
    let store = Arc::new(Mutex::new(LocalDetectionStore::new()));
    manager.set_local_detection_store(Arc::clone(&store));
    store
}

#[wasm_bindgen]
impl WasmConversationsManager {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let manager = ConversationsManager::new();
        // Register the SMTP backend so the Smtp rail resolves at ingest time.
        // self_address is only read by `send` (unwired here), so empty is fine
        // for the receive path — the shared live cell keeps the shape uniform
        // with `with_conversations`.
        let self_address = SelfAddress::new("");
        manager.register_backend(Arc::new(SmtpBackend::new_shared(
            Arc::new(ReceiveOnlySink),
            Arc::clone(&self_address),
        )));
        let local_detections = install_local_detection_store(&manager);
        Self {
            manager,
            standing_keypairs: Default::default(),
            recipient_public: Default::default(),
            mail_epoch_roots: Default::default(),
            // No transport on the client-less constructor → no bridged rail.
            #[cfg(target_arch = "wasm32")]
            bridged: RefCell::new(None),
            #[cfg(target_arch = "wasm32")]
            bridged_cursor: Default::default(),
            seen: RefCell::new(HashSet::new()),
            notif_tracker: MessageNotificationTracker::new(),
            spam_scorer: RefCell::new(None),
            local_detections,
            self_address,
            // No MLS backend on the client-less constructor → nothing to
            // register a witness on, and no roster to sweep.
            #[cfg(target_arch = "wasm32")]
            succession_witness: RefCell::new(None),
            #[cfg(target_arch = "wasm32")]
            peer_anchor_harvest: Default::default(),
            #[cfg(target_arch = "wasm32")]
            peer_anchor_sweep: Rc::new(RefCell::new(None)),
            fauna_mls: RefCell::new(None),
            conv_cursors: Rc::new(RefCell::new(HashMap::new())),
            folder_cursors: Rc::new(RefCell::new(HashMap::new())),
            scheduling_cursors: Rc::new(RefCell::new(HashMap::new())),
            scheduling_successions: fauna_client_caldav::SuccessionMemo::new(),
            #[cfg(target_arch = "wasm32")]
            nest_client: RefCell::new(None),
            // No transport on the client-less constructor → no drafts sync.
            #[cfg(target_arch = "wasm32")]
            drafts_sync: None,
            // No transport / MLS engine on the client-less constructor → no MLS sync.
            #[cfg(target_arch = "wasm32")]
            mls_sync: None,
        }
    }

    /// Update the logged-in account's canonical `<handle>@<domain>` — THE one
    /// self-heal call (`conversations.md` § State & data shape → *Self-address:
    /// live, never baked*), the web twin of
    /// `ConversationsSession::set_self_address`. Both rails read the live cell
    /// at use time, so the next SMTP send carries this `From:` (a manager built
    /// in the identity-resolution race window stops refusing `no_handle`), the
    /// FaunaMls data plane routes same-nest peers against this domain, and the
    /// reply-all self-drop compares against this address. The SPA calls it from
    /// its identity-store subscription — login resolution, the background
    /// refresh, a server-side handle rename. Idempotent; nothing is rebuilt.
    #[wasm_bindgen(js_name = setSelfAddress)]
    pub fn set_self_address(&self, self_address: String) {
        self.self_address.set(self_address);
    }

    // ── Compose surface (new-thread SMTP send) ──────────────────────────
    //
    // Thin sync wrappers over the shared `ConversationsManager` new-thread
    // compose state. The Svelte compose component (`ConversationsCompose.svelte`)
    // re-reads `snapshot()` after each call to render the recipient picker /
    // resolve status / chips — observer-driven off the snapshot, no client-side
    // state machine (`docs/goal/ui/conversations.md` § Architectural rules).
    // For an email recipient every step is rpc-free except the final send: the
    // rail decision (`address.rs::try_parse_typed_address`, any `@` → `Rail::Smtp`)
    // is local + synchronous; only `sendNewThread` awaits the `OutboundMailSink`.

    /// Begin a new-thread compose (mirrors clicking `new-conversation-button`).
    #[wasm_bindgen(js_name = startNewConversation)]
    pub fn start_new_conversation(&self) {
        self.manager.start_new_conversation();
    }

    /// Discard the in-progress new-thread compose.
    #[wasm_bindgen(js_name = cancelNewConversation)]
    pub fn cancel_new_conversation(&self) {
        self.manager.cancel_new_conversation();
    }

    /// Set the recipient-picker raw input. Drives `recipient-picker-input`.
    #[wasm_bindgen(js_name = setNewThreadRecipientInput)]
    pub fn set_new_thread_recipient_input(&self, text: String) {
        self.manager.set_new_thread_recipient_input(text);
    }

    /// Commit the current input as a recipient chip (`recipient-picker-chip`).
    /// Returns whether a chip was added. For an email this uses the local
    /// shape-parse fallback, so it works whether or not `resolveRecipient` ran.
    #[wasm_bindgen(js_name = acceptCurrentRecipientChip)]
    pub fn accept_current_recipient_chip(&self) -> bool {
        self.manager.accept_current_recipient_chip()
    }

    /// Commit a known Fauna actor (`handle` + 64-hex `actor_id`) directly as a
    /// new-thread recipient chip — the boundary form of `accept_new_thread_chip`
    /// for a typed-Fauna recipient (twin of [`Self::accept_add_participant_fauna_chip`]
    /// for the new-thread picker). A *synchronous* seed with no rail probe: the
    /// caller already holds the actor_id (e.g. the Profile page's start-DM glue),
    /// so unlike the GUI's resolve-then-`acceptCurrentRecipientChip` path it needs
    /// no `resolveRecipient` round-trip; the MLS group bootstraps lazily on the
    /// first send. Mirrors linux `profile::start_dm`'s `accept_new_thread_chip`.
    #[wasm_bindgen(js_name = acceptNewThreadFaunaChip)]
    pub fn accept_new_thread_fauna_chip(
        &self,
        handle: String,
        actor_id_hex: String,
    ) -> Result<(), JsValue> {
        let actor_id = parse_actor_hex(&actor_id_hex)?;
        self.manager
            .accept_new_thread_chip(TypedAddress::Fauna { handle, actor_id });
        Ok(())
    }

    /// Set the subject draft (`None` = topic collapsed; `Some("")` = expanded
    /// but blank). Drives `topic-toggle-button` + `subject-input`.
    #[wasm_bindgen(js_name = setNewThreadSubject)]
    pub fn set_new_thread_subject(&self, subject: Option<String>) {
        self.manager.set_new_thread_subject(subject);
    }

    /// Set the body draft. Drives `dm-text-field`.
    #[wasm_bindgen(js_name = setNewThreadBody)]
    pub fn set_new_thread_body(&self, body: String) {
        self.manager.set_new_thread_body(body);
    }

    // ── Attachments (compose) ──────────────────────────────────────────────
    // Thin passthroughs to the shared manager's attachment store, so the web
    // compose path is byte-identical to native (priority #2): the SPA reads the
    // picked file's bytes (a `File`/`Blob` → `Uint8Array`) and stages them here;
    // `send` / `sendNewThread` resolve them to the backend (SMTP inlines the
    // bytes in the MIME; FaunaMls seals + uploads — `docs/goal/ui/conversations.md`
    // § Attachments). The observed snapshot stays byte-free (the staged draft is
    // light), so a multi-MB image never crosses the snapshot boundary.

    /// Stage an attachment on an existing thread's compose draft, caching its
    /// bytes under the returned `blob_hash` (BLAKE3 of the bytes). Drives
    /// `attachment-button`.
    #[wasm_bindgen(js_name = addAttachment)]
    pub fn add_attachment(
        &self,
        thread_id: String,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> String {
        self.manager
            .add_attachment(ThreadId(thread_id), filename, mime_type, bytes)
    }

    /// Remove the staged attachment at `index` from an existing thread's compose.
    #[wasm_bindgen(js_name = removeAttachment)]
    pub fn remove_attachment(&self, thread_id: String, index: u32) {
        self.manager.remove_attachment(ThreadId(thread_id), index);
    }

    /// Stage an attachment on the open new-thread compose. `None` if no
    /// new-thread compose is open; else the staged `blob_hash`.
    #[wasm_bindgen(js_name = addNewThreadAttachment)]
    pub fn add_new_thread_attachment(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Option<String> {
        self.manager
            .add_new_thread_attachment(filename, mime_type, bytes)
    }

    /// Remove the staged attachment at `index` from the new-thread compose.
    #[wasm_bindgen(js_name = removeNewThreadAttachment)]
    pub fn remove_new_thread_attachment(&self, index: u32) {
        self.manager.remove_new_thread_attachment(index);
    }

    /// The shared attachment loader: the plaintext bytes cached under
    /// `blob_hash`, or `None` for an unknown / not-yet-fetched hash. The web
    /// render glue resolves a `dm-attachment-image[i]` / `-file[i]` to real bytes
    /// through this (e.g. a `Blob` URL) — the same handle the snapshot carries.
    #[wasm_bindgen(js_name = attachmentBytes)]
    pub fn attachment_bytes(&self, blob_hash: String) -> Option<Vec<u8>> {
        self.manager.attachment_bytes(blob_hash)
    }

    /// Whether `blob_hash`'s bytes are resident right now — a read-only peek (no
    /// copy, no miss recorded) the render glue checks before serving a blob URL
    /// it made earlier, since that URL outlives an eviction of the bytes.
    #[wasm_bindgen(js_name = attachmentResident)]
    pub fn attachment_resident(&self, blob_hash: String) -> bool {
        self.manager.attachment_resident(blob_hash)
    }

    /// Stash the account's complete standing recipient-mail key set the inbound
    /// poll opens sealed records with — a flat buffer of `k × (32 + 2400)` bytes
    /// (`x25519_secret ∥ mlkem_dk` per generation, current first, then grace)
    /// from `WasmMailSettingsMachine::recipientStandingKeypairs`, derived from
    /// the `fauna.state.mail` row's `{msek, prior_mseks}` in wasm. Idempotent; an empty
    /// buffer clears the set (mail disabled). Must be set before
    /// [`Self::ingest_sealed_inbound`] can open a record. Rejects a length that
    /// is not a whole number of keypairs.
    #[wasm_bindgen(js_name = setRecipientKeypairs)]
    pub fn set_recipient_keypairs(&self, flat: Vec<u8>) -> Result<(), JsValue> {
        const LEN: usize = 32 + fauna_mls::wrapped_blob::MLKEM768_DECAPS_KEY_LEN;
        if !flat.len().is_multiple_of(LEN) {
            return Err(JsValue::from_str(
                "recipient keypairs must be a whole number of (32 + 2400)-byte entries",
            ));
        }
        let parsed = flat
            .chunks_exact(LEN)
            .map(|chunk| {
                let (sk, dk) = chunk.split_at(32);
                fauna_mls::wrapped_blob::StandingMailKeypair::new_hybrid(
                    sk.try_into().expect("chunk_exact: 32-byte X25519 half"),
                    dk.try_into().expect("chunk_exact: 2400-byte ML-KEM half"),
                )
            })
            .collect();
        *self.standing_keypairs.borrow_mut() = parsed;
        Ok(())
    }

    /// Stash the account's own X-Wing recipient **public** key (1216 B, from
    /// `WasmMailSettingsMachine::recipientPublicKey`) — what the bridged rail
    /// seals the user's own copy of a sent message to. Set beside
    /// [`Self::set_recipient_keypairs`]; an empty buffer clears it (mail
    /// disabled). Rejects bytes that are not a valid key.
    #[wasm_bindgen(js_name = setRecipientPublicKey)]
    pub fn set_recipient_public_key(&self, public: Vec<u8>) -> Result<(), JsValue> {
        let parsed = if public.is_empty() {
            None
        } else {
            Some(
                fauna_mls::wrapped_blob::XWingPublicKey::parse_and_validate(&public)
                    .map_err(|e| JsValue::from_str(&format!("recipient public key: {e}")))?,
            )
        };
        *self.recipient_public.borrow_mut() = parsed;
        Ok(())
    }

    /// True once the standing key set has been set — the JS poll loop gates its
    /// `fauna.email.inbox.fetch` on this (no point fetching before mail is
    /// enabled / the set is derivable).
    #[wasm_bindgen(js_name = hasRecipientSecret)]
    pub fn has_recipient_secret(&self) -> bool {
        !self.standing_keypairs.borrow().is_empty()
    }

    /// How many received mail records this page's receive loop skipped because
    /// they would not open under the account's key set — the shared
    /// `ConversationsManager::unopenable_mail_count`, for the page's
    /// `error-message` projection (`ui/conversations.md` § Errors & edge cases).
    #[wasm_bindgen(js_name = unopenableMailCount)]
    pub fn unopenable_mail_count(&self) -> u32 {
        self.manager.unopenable_mail_count()
    }

    /// Stash the client's **mail-epoch roots** — a flat `N × 32`-byte buffer from
    /// `WasmMailSettingsMachine::mailEpochRoots` (current MSEK's root then one per
    /// prior grace generation). Set alongside [`Self::set_recipient_keypairs`] when
    /// mail is seen enabled so [`Self::ingest_sealed_inbound`] can open mail
    /// sealed under a mail epoch key (content-sealing-epochs, design § 4/§ 5).
    /// An empty buffer clears them (mail disabled) → the open falls back to the
    /// standing recipient secret, unchanged. Idempotent. Rejects a length that
    /// is not a multiple of 32.
    #[wasm_bindgen(js_name = setMailEpochRoots)]
    pub fn set_mail_epoch_roots(&self, roots: Vec<u8>) -> Result<(), JsValue> {
        if !roots.len().is_multiple_of(32) {
            return Err(JsValue::from_str(
                "mail epoch roots must be a whole number of 32-byte roots",
            ));
        }
        let parsed: Vec<[u8; 32]> = roots
            .chunks_exact(32)
            .map(|c| {
                let mut r = [0u8; 32];
                r.copy_from_slice(c);
                r
            })
            .collect();
        *self.mail_epoch_roots.borrow_mut() = parsed;
        Ok(())
    }

    /// Open ONE sealed inbound record (as shipped by `fauna.email.inbox.fetch`)
    /// and ingest its decrypted RFC 5322 into the manager on the Smtp rail.
    /// Returns `true` when a new message was ingested, `false` when the record
    /// was already seen (dedup) or carried no usable `From` (skipped, not fatal).
    ///
    /// `internal_date_secs` is the feed's epoch-**seconds** `internal_date` (the
    /// rail model wants ms). Rejects if no recipient MSEK has been set, or if the
    /// two-layer HPKE open fails (wrong key / corrupt envelope) — the JS loop
    /// retries that record on the next tick (it isn't marked seen on failure).
    ///
    /// `flags` is the message's IMAP flag/keyword set (from the `inbox.fetch`
    /// reply's per-message `flags`); `score` is `true` only for the **INBOX**
    /// feed (never Sent). When both `score` is set and on-device spam scoring is
    /// active ([`Self::enable_spam_scoring`]), the just-decrypted message is fed
    /// to the held [`InboxSpamScorer`] — skipped if already `$FaunaSpamScored`-
    /// watermarked — so the decrypted body is scored in-place without ever
    /// crossing into JS (`mail-spam.md` § Scoring placement). The disposition is
    /// applied once per pass via [`Self::take_spam_disposition`]. A message the
    /// scorer classifies as spam is **not** surfaced in the thread view (it is
    /// about to move INBOX→Junk) and `false` is returned; ham + already-watermarked
    /// messages ingest normally. Returns `true` when the record was newly ingested
    /// into the view.
    ///
    /// `mailbox` names the feed the record was read from — `"inbox"` or
    /// `"sent"` ([`MailFeed`]'s serialized spelling). It rides beside `uid` as
    /// the attachment store's coordinates for this record's attachments, so an
    /// evicted one is re-read from the right mailbox
    /// ([`Self::take_wanted_mail_records`]); UIDs are per mailbox.
    ///
    /// A freshly-delivered INBOX record the scorer did NOT file Junk is also
    /// handed to the shared mailed-REPLY merge ([`spawn_reply_merge`], the twin
    /// of the native `NestMailInboundSource::spawn_reply_merge`), so a web-only
    /// organizer's stored event takes an attendee's mailed answer
    /// (`inbound-scheduling-authority.md` § The mail rail). `secret_hex` rides in
    /// per call for that merge's MSEK lookup (the manager holds no identity
    /// seed), and `now_secs` from JS under the wasm-time discipline — `f64`, not
    /// `i64`, so the JS side passes a plain number.
    #[wasm_bindgen(js_name = ingestSealedInbound)]
    #[allow(clippy::too_many_arguments)]
    pub fn ingest_sealed_inbound(
        &self,
        uid: u32,
        message_id: Vec<u8>,
        internal_date_secs: i64,
        stored_at_secs: f64,
        sealed_envelope: Vec<u8>,
        flags: Vec<String>,
        score: bool,
        mailbox: String,
        secret_hex: String,
        now_secs: f64,
    ) -> Result<bool, JsValue> {
        if self.seen.borrow().contains(&message_id) {
            return Ok(false);
        }
        let mailbox = mail_feed_from_js(&mailbox)?;
        let rfc5322 = match open_sealed_mail_record(self, uid, stored_at_secs, &sealed_envelope) {
            Ok(rfc5322) => rfc5322,
            // Deterministic for this key set (`OpenMiss::Unopenable`): skipped,
            // never blocking — marked seen so the drain moves on, and recorded on
            // the shared manager so the page can tell the user. The same rule the
            // native page opener applies (`mail-app-surface.md` § Inbound client
            // receive → *Unopenable records*).
            Err(OpenMiss::Unopenable(reason)) => {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "inbound mail record could not be opened; skipped (uid {uid}): {reason}"
                )));
                self.seen.borrow_mut().insert(message_id);
                self.manager.note_unopenable_mail(mailbox, uid);
                return Ok(false);
            }
            // No key set yet — the JS loop gates on `hasRecipientSecret`, so this
            // is a caller bug, not a record fact; surface it as before.
            Err(OpenMiss::NoKeys) => {
                return Err(JsValue::from_str(
                    "mail not enabled: recipient MSEK not set",
                ));
            }
        };
        self.manager.retire_unopenable_mail(mailbox, uid);
        // Score-at-ingest (INBOX only): feed the just-decrypted body to the shared
        // scorer BEFORE it moves into the record. A watermarked message is skipped
        // inside `observe`. Scores once per freshly-seen message (the dedup check
        // above already skipped re-polls of a message scored this session; the
        // watermark skips one scored in a prior session or by the MDA).
        let mut junked = false;
        if score && let Some(scorer) = self.spam_scorer.borrow_mut().as_mut() {
            let text = String::from_utf8_lossy(&rfc5322);
            junked = scorer.observe(uid, &flags, &text);
        }
        // A message the on-device scorer just classified as spam is kept OUT of the
        // inbox thread view: `flush_spam_scoring`'s `apply_spam_disposition` moves it
        // INBOX→Junk on the nest this same pass, so surfacing it here would show the
        // user mail that has already been filed away. This is the client-side twin of
        // the MDA moving spam out of INBOX *before* the `SELECT` snapshot
        // (`mail-spam.md` § Re-file timing). Marked seen (like a successful ingest) so
        // a re-poll before the Junk move lands can't re-surface it; the accumulated
        // junk UID still rides out via `take_spam_disposition`.
        if junked {
            self.seen.borrow_mut().insert(message_id);
            return Ok(false);
        }
        // Then route an inbound iTIP REPLY to the calendar layer — INBOX only
        // (Sent is never REPLY-merged), never from a copy the scorer just filed
        // Junk (caldav-server.md § Server-side auto-schedule, invitation rule 3).
        // The `seen` check above keeps a re-poll from merging twice this
        // session; across sessions the merge is idempotent.
        #[cfg(target_arch = "wasm32")]
        if mailbox == MailFeed::Inbox {
            self.spawn_reply_merge(&rfc5322, secret_hex, now_secs);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (secret_hex, now_secs);
        let record = InboundMailRecord {
            uid,
            message_id: message_id.clone(),
            internal_date_ms: internal_date_secs.saturating_mul(1000),
            rfc5322,
            // The wasm manager suppresses a junk verdict earlier (the `junked`
            // early-return above), so a record that reaches here is always shown.
            suppress_from_view: false,
            has_seen_flag: fauna_conversations::carries_seen_flag(&flags),
            mailbox,
        };
        let ingested = ingest_inbound_record(&self.manager, &record)
            .map_err(|e| JsValue::from_str(&format!("ingest inbound (uid {uid}): {e}")))?;
        // Mark seen only after a successful open+ingest so a transient failure
        // re-polls rather than silently dropping the record.
        self.seen.borrow_mut().insert(message_id);
        Ok(ingested)
    }

    /// Merge one mailed iTIP REPLY into the organizer's stored event,
    /// best-effort, off the ingest path — the web twin of the native
    /// `NestMailInboundSource::spawn_reply_merge`. The raw bytes never leave
    /// wasm: the shared `CalDavClient::apply_inbound_reply_from_mail` owns the
    /// "is this a schedulable REPLY?" gate and the authenticated-sender check
    /// (`inbound-scheduling-authority.md` § The mail rail), so ordinary mail is
    /// a cheap `NotCalendarReply` no-op. A `Refused` outcome is `console.warn`ed
    /// and recorded on the session's refused-change inbox (the row composed by
    /// `InboundReplyOutcome::refused_mail_change_record` over the same bytes),
    /// exactly as [`WebSchedulingSink`] records a sealed-rail refusal. Errors
    /// are logged, never surfaced — the mail itself already delivered. A no-op
    /// with no wired client or with mail/CalDAV off (no MSEK).
    #[cfg(target_arch = "wasm32")]
    fn spawn_reply_merge(&self, rfc5322: &[u8], secret_hex: String, now_secs: f64) {
        let Some(client) = self.nest_client.borrow().clone() else {
            return;
        };
        let raw = rfc5322.to_vec();
        let refused = self.manager.refused_changes();
        let now_secs = now_secs as i64;
        wasm_bindgen_futures::spawn_local(async move {
            let DavStoreContext {
                actor_id,
                msek,
                prior_mseks,
            } = match crate::rpc::dav_ctx(&secret_hex).await {
                Ok(Some(ctx)) => ctx,
                Ok(None) => return,
                Err(e) => {
                    web_sys::console::error_1(&JsValue::from_str(&format!(
                        "inbound reply merge: key context failed: {e:?}"
                    )));
                    return;
                }
            };
            match fauna_client_caldav::CalDavClient::new(client)
                .apply_inbound_reply_from_mail(&actor_id, &msek, &prior_mseks, &raw, now_secs)
                .await
            {
                Ok(outcome) => {
                    if let Some(row) = outcome.refused_mail_change_record(&raw, now_secs) {
                        web_sys::console::warn_1(&JsValue::from_str(&format!(
                            "inbound mailed REPLY refused ({}): {}",
                            row.sender_address, row.reason
                        )));
                        refused.record(row);
                    }
                }
                Err(e) => web_sys::console::error_1(&JsValue::from_str(&format!(
                    "inbound reply merge failed: {e}"
                ))),
            }
        });
    }

    /// Resolve a feed message's `body_ref` back to the sealed outer envelope it
    /// stands for, so [`Self::ingest_sealed_inbound`] can open it as if it had
    /// arrived inline — the web half of the client-feed reference leg
    /// (`smtp-server.md` § Message size limits).
    ///
    /// A message whose stored outer envelope exceeds the 2 MiB WS-RPC frame
    /// cannot ride `inbox.fetch` inline: the reply carries an empty
    /// `sealed_envelope` plus an `InboxMessage.body_ref`, and the bytes wait on
    /// the byte plane. The JS drain calls this first when `body_ref` is present
    /// and feeds the result straight to `ingestSealedInbound`; below the frame
    /// there is no `body_ref` and the drain is unchanged. Keeping the fetch here
    /// rather than inside `ingestSealedInbound` is what lets that one stay
    /// synchronous — its five other call shapes are untouched.
    ///
    /// `chunk_hashes_concat` is the reference's ordered hashes **concatenated**
    /// (32 bytes each, exactly as `MailBodyRef.chunk_hashes` carries them; the
    /// shared resolver width-checks every one and refuses a short tail).
    /// `total_bytes` is `MailBodyRef.total_bytes` — an `f64` rather than a `u64`
    /// so JS passes a plain number instead of a `BigInt` (a mail body is bounded
    /// far below 2^53). The whole fail-closed rule — hash width, fetch, and the
    /// declared total — lives in `fauna_mail::body_ref`, shared with the native
    /// receive path, so no target can drift.
    ///
    /// Rejects if the manager has no wired client (the receive-only constructor).
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = resolveMailBodyRef)]
    pub fn resolve_mail_body_ref(
        &self,
        chunk_hashes_concat: Vec<u8>,
        total_bytes: f64,
    ) -> Result<js_sys::Promise, JsValue> {
        let nest_url = self
            .nest_client
            .borrow()
            .as_ref()
            .map(|c| c.nest_url())
            .ok_or_else(|| JsValue::from_str("resolve body ref: no nest client wired"))?;
        Ok(wasm_bindgen_futures::future_to_promise(async move {
            let bytes = resolve_mail_body_ref_inner(&nest_url, &chunk_hashes_concat, total_bytes)
                .await
                .map_err(|e| JsValue::from_str(&format!("resolve body ref: {e}")))?;
            Ok(js_sys::Uint8Array::from(bytes.as_slice()).into())
        }))
    }

    /// Hand the shared manager one `INBOX` page's `highest_modseq` — the web
    /// drain's twin of the native page loop's baseline offer
    /// (`backends::smtp::poll_inbound_mail`). The manager keeps the first
    /// non-zero one as the flag-change cursor's start (`mail-app-surface.md`
    /// § Read state); zero (an inbox with no modseq) offers nothing. An
    /// `f64` so JS passes a plain number (a modseq stays far below 2^53).
    #[wasm_bindgen(js_name = noteInboxPage)]
    pub fn note_inbox_page(&self, highest_modseq: f64) {
        if highest_modseq >= 1.0 {
            self.manager.offer_mail_flag_baseline(highest_modseq as u64);
        }
    }

    /// Whether a thread read owes the nest `\Seen` writes not yet sent — the
    /// web twin of the native session's mail-read poke: the JS page asks after
    /// each action and wakes the mail rail when it is `true`.
    #[wasm_bindgen(js_name = hasOwedMailSeen)]
    pub fn has_owed_mail_seen(&self) -> bool {
        self.manager.has_owed_mail_seen()
    }

    /// Bring mail read state level with the nest: the owed `\Seen` writes out
    /// in one batch, the flag changes made elsewhere in — the SAME shared
    /// driver every native app's receive loop runs
    /// (`backends::smtp::sync_mail_read_state`), over this manager's
    /// WS-RPC client. The web receive pass calls it after its `INBOX` drain,
    /// and the `fauna.mail.flags_changed` push wakes that pass. Never rejects:
    /// a failure stays owed or un-drained for the next pass, and an
    /// `Unsupported` source (a non-INBOX or default one) ends syncing silently. Resolves at once with no wired client.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = syncMailReadState)]
    pub fn sync_mail_read_state(&self) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let manager = Arc::clone(&self.manager);
        future_to_promise(async move {
            if let Some(client) = client {
                let source = WebInboxReadState {
                    email: fauna_client_email::EmailClient::new(client),
                };
                fauna_conversations::backends::smtp::sync_mail_read_state(&source, &manager).await;
            }
            Ok(JsValue::UNDEFINED)
        })
    }

    /// The mail records whose evicted attachments a render has missed since the
    /// last pass — `[{ record: { mailbox, uid }, blobHashes }]`, one entry per
    /// record — for the JS poll to re-read by `(mailbox, uid)` and hand back to
    /// [`Self::refill_mail_record`], or to [`Self::mail_record_gone`] when the
    /// mailbox no longer holds it. The web half of the SMTP attachment refill
    /// (`conversations.md` § Attachments → *Retention*): the drain and the
    /// per-record rule are `fauna_conversations::backends::smtp`'s, shared with
    /// the native mail sweep; only the fetch runs in JS, because web's WS-RPC
    /// client cannot ride the `InboundMailSource` seam. FaunaMls wants stay for
    /// `pollConversations`.
    #[wasm_bindgen(js_name = takeWantedMailRecords)]
    pub fn take_wanted_mail_records(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&take_wanted_mail_records(&self.manager))
    }

    /// Cache again the wanted attachments of one re-read mail record. Opens the
    /// sealed record exactly as [`Self::ingest_sealed_inbound`] does (same keys,
    /// same seal-instant rule; the caller resolves a `body_ref` first, as the
    /// drain does), then re-parses and caches through the shared per-record
    /// refill, which notifies observers when anything was cached and forgets a
    /// wanted handle the record no longer yields. `want` is the entry
    /// [`Self::take_wanted_mail_records`] handed out. Returns how many
    /// attachments were cached again. Rejects when the record does not open —
    /// its handles stay remembered, and the next render's miss asks again.
    #[wasm_bindgen(js_name = refillMailRecord)]
    pub fn refill_mail_record(
        &self,
        want: JsValue,
        stored_at_secs: f64,
        sealed_envelope: Vec<u8>,
    ) -> Result<u32, JsValue> {
        let want: WantedMailRecord = serde_wasm_bindgen::from_value(want)
            .map_err(|e| JsValue::from_str(&format!("refill mail record: {e}")))?;
        let rfc5322 =
            open_sealed_mail_record(self, want.record.uid, stored_at_secs, &sealed_envelope)?;
        Ok(refill_mail_record_attachments(&self.manager, &want, Some(&rfc5322)) as u32)
    }

    /// The mailbox no longer holds `want`'s record (moved to Junk, expunged):
    /// forget its handles, so they render declared instead of being asked for
    /// on every pass.
    #[wasm_bindgen(js_name = mailRecordGone)]
    pub fn mail_record_gone(&self, want: JsValue) -> Result<(), JsValue> {
        let want: WantedMailRecord = serde_wasm_bindgen::from_value(want)
            .map_err(|e| JsValue::from_str(&format!("mail record gone: {e}")))?;
        refill_mail_record_attachments(&self.manager, &want, None);
        Ok(())
    }

    /// Enable on-device INBOX spam scoring for this actor's coming drain pass(es).
    ///
    /// `sealed_model` is the still-sealed per-user model from
    /// `fauna.bridges.fetch_spam_model` — a **bare inner** `wrapped_blob`
    /// (`open_sealed_inner_record` shape, NOT the two-layer `inbox.fetch` feed):
    /// this unwraps it under the held recipient secret (the model plaintext never
    /// crosses into JS) and constructs the shared [`InboxSpamScorer`] with the
    /// admin-effective `spam_folder` threshold + Bayesian knobs from
    /// `fauna.bridges.get_spam_scoring_policy` (so the client's Junk line matches
    /// the MDA/nest — `mail-spam.md` § Architectural rules). The JS loop calls
    /// this only when `fetch_spam_model` returned a model (a *trained* actor); an
    /// untrained actor calls [`Self::disable_spam_scoring`] instead, so no
    /// un-based watermarking happens.
    #[wasm_bindgen(js_name = enableSpamScoring)]
    pub fn enable_spam_scoring(
        &self,
        sealed_model: Vec<u8>,
        spam_folder_threshold: u32,
        bayesian_weight_milli: u32,
        bayesian_min_samples: u32,
        bayesian_full_confidence_samples: u32,
        baseline: Option<Vec<u8>>,
    ) -> Result<(), JsValue> {
        // The model is sealed to the CURRENT generation's keypair (re-sealed on
        // every training write), so only the first entry applies.
        let keypairs = self.standing_keypairs.borrow();
        let current = keypairs
            .first()
            .ok_or_else(|| JsValue::from_str("mail not enabled: recipient MSEK not set"))?;
        let model_bytes = match current.mlkem_dk.as_deref() {
            Some(mlkem_dk) => fauna_mail::open_sealed_inner_record_hybrid(
                &sealed_model,
                &current.x25519_secret,
                mlkem_dk,
            ),
            None => fauna_mail::open_sealed_inner_record(&sealed_model, &current.x25519_secret),
        }
        .map_err(|e| JsValue::from_str(&format!("unwrap spam model: {e}")))?;
        // A client-sealed stored model rides the published deployment baseline
        // on the fetch reply for the AGENT to fold (a plaintext-stored model
        // was folded nest-side and passes `None` — the no-double-fold rule,
        // `mail-spam.md` § Encrypted-mode interaction).
        let model_bytes = fauna_mail::spam::fold_spam_model_baseline(
            model_bytes,
            baseline.unwrap_or_default(),
            bayesian_full_confidence_samples,
        );
        let knobs = BayesianKnobs {
            bayesian_weight_milli,
            min_samples: bayesian_min_samples,
            full_confidence_samples: bayesian_full_confidence_samples,
        };
        *self.spam_scorer.borrow_mut() = Some(InboxSpamScorer::new(
            model_bytes,
            spam_folder_threshold,
            knobs,
        ));
        Ok(())
    }

    /// Disable on-device spam scoring — cold start (no trained model) or mail
    /// disabled — so [`Self::ingest_sealed_inbound`] scores nothing. Idempotent.
    #[wasm_bindgen(js_name = disableSpamScoring)]
    pub fn disable_spam_scoring(&self) {
        *self.spam_scorer.borrow_mut() = None;
    }

    /// Drain the scored-this-pass disposition — `{ scoredUids, junkUids }` — to
    /// feed one `fauna.email.apply_spam_disposition` (watermark all scored, move
    /// the junk subset INBOX→Junk). Empty lists when scoring is disabled or
    /// nothing scored (the JS loop then skips the RPC). Leaves the scorer reusable
    /// for the next pass.
    #[wasm_bindgen(js_name = takeSpamDisposition)]
    pub fn take_spam_disposition(&self) -> Result<JsValue, JsValue> {
        let (scored_uids, junk_uids) = match self.spam_scorer.borrow_mut().as_mut() {
            Some(scorer) => scorer.take(),
            None => (Vec::new(), Vec::new()),
        };
        crate::rpc::to_js(&SpamDispositionJs {
            scored_uids,
            junk_uids,
        })
    }

    /// The conversations snapshot (threads + sort + selection) as a plain JS
    /// object — the SPA renders the thread list from `snapshot.threads`, whose
    /// `rail` serializes to `"Smtp"` / `"FaunaMls"` / … .
    #[wasm_bindgen(js_name = "snapshot")]
    pub fn snapshot(&self) -> Result<JsValue, JsValue> {
        // `json_compatible` so threads render as plain JS objects with
        // numbers-as-numbers (matches the admin machines' `to_js`); the `rail`
        // unit-enum variant serializes to the string "Smtp".
        crate::rpc::to_js(&self.manager.snapshot())
    }

    /// The bound nest-channel id (hex) for a FaunaMls thread once its MLS group
    /// has bootstrapped, else `None`. Sourced from the manager's channel binding
    /// (not [`Self::thread_detail`] — `ThreadDetail` carries no channel id), the
    /// browser twin of linux `state.rs`'s `manager.channel_hex(thread_id)` row.
    /// Drives the `channel_id_hex` field of the e2e `data.conversation_threads`
    /// rows the tier_3 real-wire test reads.
    #[wasm_bindgen(js_name = channelHex)]
    pub fn channel_hex(&self, thread_id: String) -> Option<String> {
        self.manager.channel_hex(&ThreadId(thread_id))
    }

    /// Per-channel counts of inbound MLS commits this page has folded in, as a
    /// `{channel_hex: count}` object — the browser leg of the cross-device
    /// barrier (`fauna_e2e_agent::MLS_FOLDED_COMMITS_KEY` owns the contract;
    /// `FaunaMlsBackend::folded_commits` the reasoning; the JSON shape is derived
    /// once in `fauna_conversations::state_json::mls_folded_commits_json`, the
    /// same function tui and linux publish).
    ///
    /// `{}` before the FaunaMls rail is wired — the manager may exist with no
    /// backend yet (the client-less constructor), which is a legitimate zero for
    /// every channel, not "this app has no leg". The `null` a non-publishing app
    /// yields comes from the *absence* of this export, and the consumer refuses
    /// that loudly (convention 11).
    #[wasm_bindgen(js_name = mlsFoldedCommits)]
    pub fn mls_folded_commits(&self) -> Result<JsValue, JsValue> {
        let value = match self.fauna_mls.borrow().clone() {
            Some(backend) => fauna_conversations::state_json::mls_folded_commits_json(&backend),
            None => serde_json::Value::Object(serde_json::Map::new()),
        };
        crate::rpc::to_js(&value)
    }

    /// The `data.succession_witness` e2e state — the member side of a
    /// succession as this seat's driver sees it: what the inbound poll did
    /// with the statements it saw, what the harvest managed per peer, and
    /// what the witness made of each identity.
    ///
    /// One call over [`fauna_client_recovery::witness::state_json`], the same
    /// renderer every other app publishes; web derives none of the shape and
    /// none of the reading order. The three inputs are field reads, never
    /// round trips — convention 11's second corollary, since the state
    /// provider is the ack path.
    ///
    /// `null` before a witness exists (the receive-only constructor, or a
    /// secret that would not parse), which is the honest reading of "this seat
    /// registered none" and is exactly what the absence means on every other
    /// app.
    #[wasm_bindgen(js_name = successionWitnessStateJson)]
    pub fn succession_witness_state_json(&self) -> Result<JsValue, JsValue> {
        let Some(witness) = self.succession_witness.borrow().clone() else {
            return Ok(JsValue::NULL);
        };
        let counts = self
            .fauna_mls
            .borrow()
            .as_ref()
            .map(|b| b.succession_statement_counts())
            .unwrap_or_default();
        crate::rpc::to_js(&fauna_client_recovery::witness::state_json(
            &witness.observation(),
            &self.peer_anchor_harvest,
            &counts,
        ))
    }

    /// The `data.conversation_threads` e2e state rows (thread_id, label,
    /// snippet, rail, flavor, unread_count, participant_count, message_count,
    /// message_subject_lines, participant_actor_ids, channel_id_hex) as a
    /// plain JS array — one call over
    /// [`fauna_conversations::state_json::conversation_threads_json`], the
    /// same function tui and linux publish (native, no wasm boundary to
    /// cross). Row assembly (the per-thread `thread_detail` join +
    /// `participant_actor_ids`' Fauna-rail-vs-null mapping) lives ONCE in
    /// that function; a caller that hand-rebuilds these fields itself is
    /// re-deriving a contract this export already computes byte-for-byte —
    /// don't.
    #[wasm_bindgen(js_name = conversationThreadsJson)]
    pub fn conversation_threads_json(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&fauna_conversations::state_json::conversation_threads_json(
            &self.manager,
        ))
    }

    /// The `data.conversation_sort` e2e state value — the list's active order
    /// in `setSort`'s serde spelling — from the same
    /// [`fauna_conversations::state_json::conversation_sort_json`] tui and linux
    /// publish, so the SPA never spells the order names itself.
    #[wasm_bindgen(js_name = conversationSortJson)]
    pub fn conversation_sort_json(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&fauna_conversations::state_json::conversation_sort_json(
            &self.manager,
        ))
    }

    /// Diff the current snapshot for **new-message OS banners**: the threads
    /// that warrant one right now, as `[{threadId, label, snippet}, …]`.
    /// Empty on the seeding tick, on an unchanged tick, and for the thread the
    /// user has open.
    ///
    /// Stateful — this is one tick of `MessageNotificationTracker`, so call it
    /// exactly once per snapshot change (web's `refreshConversations` is that
    /// chokepoint) and never speculatively: a call is a tick, and a tick the
    /// caller discards is a banner the user will never be shown.
    ///
    /// The decision is the shared tracker's three rules, identical on every app
    /// (`conversations.md` § Where logic lives); what the SPA owes is the
    /// *firing* — one `new Notification()` per entry — exactly as linux owes the
    /// freedesktop call and windows the WinUI one. The selected thread is
    /// suppressed inside the tracker, so the caller passes no exclusion of its
    /// own and must add none.
    #[wasm_bindgen(js_name = newMessageBanners)]
    pub fn new_message_banners(&self) -> Result<JsValue, JsValue> {
        let snapshot = self.manager.snapshot();
        let activities: Vec<ThreadActivity> = snapshot
            .threads
            .iter()
            .map(ThreadActivity::from_summary)
            .collect();
        let to_fire = self.notif_tracker.diff(
            activities,
            snapshot.selected_thread_id.clone(),
            snapshot.launch_floor_ms,
        );
        let rows: Vec<serde_json::Value> = to_fire
            .into_iter()
            .map(|a| {
                serde_json::json!({
                    "threadId": a.thread_id.0,
                    "label": a.label,
                    "snippet": a.snippet,
                })
            })
            .collect();
        crate::rpc::to_js(&rows)
    }

    /// The full [`ThreadDetail`] for one thread (messages + compose +
    /// capabilities + participants) as a plain JS object, or `null` if the id is
    /// unknown. The detail pane renders the message stream + compose bar from it.
    #[wasm_bindgen(js_name = threadDetail)]
    pub fn thread_detail(&self, thread_id: String) -> Result<JsValue, JsValue> {
        match self.manager.thread_detail(ThreadId(thread_id)) {
            Some(detail) => crate::rpc::to_js(&detail),
            None => Ok(JsValue::NULL),
        }
    }

    /// What the compose bar previews for the reply in progress
    /// (`dm-reply-preview`) — `{sender_display, excerpt}` from the shared
    /// [`ConversationsManager::reply_preview`], or `null` when no reply is armed
    /// or the answered message is not in the fetched window. The SPA renders the
    /// record and never derives it.
    #[wasm_bindgen(js_name = replyPreview)]
    pub fn reply_preview(&self, thread_id: String) -> Result<JsValue, JsValue> {
        match self.manager.reply_preview(ThreadId(thread_id)) {
            Some(preview) => crate::rpc::to_js(&preview),
            None => Ok(JsValue::NULL),
        }
    }

    // ── List-pane controls: sort & search (snapshot-only mutators) ──────
    //
    // `conversation-sort` / `conversation-search-box` (conversations.md §
    // User actions). `setSort` reorders `snapshot().threads`; `setSearchQuery`
    // plumbs the query into `snapshot().search_query` (global thread-list
    // filtering is a deferred follow-on — spec line 659).

    /// Set the thread-list sort order (`conversation-sort`). `order` is the
    /// serde string the snapshot's `sort` field round-trips —
    /// `"LatestActivity"` / `"OldestFirst"` / `"Unread"`. Errors on an unknown
    /// value rather than silently defaulting.
    #[wasm_bindgen(js_name = setSort)]
    pub fn set_sort(&self, order: String) -> Result<(), JsValue> {
        let sort = match order.as_str() {
            "LatestActivity" => SortOrder::LatestActivity,
            "OldestFirst" => SortOrder::OldestFirst,
            "Unread" => SortOrder::Unread,
            other => return Err(JsValue::from_str(&format!("unknown sort order: {other}"))),
        };
        self.manager.set_sort(sort);
        Ok(())
    }

    /// Set the thread-list search query (`conversation-search-box`). `None`
    /// (JS `undefined`/empty) clears it. Stored on the snapshot; filtering is
    /// deferred (see above).
    #[wasm_bindgen(js_name = setSearchQuery)]
    pub fn set_search_query(&self, query: Option<String>) {
        // Treat an empty string as "cleared" so an emptied search box restores
        // the unfiltered list rather than storing `Some("")`.
        let query = query.filter(|q| !q.is_empty());
        self.manager.set_search_query(query);
    }

    /// Opt a message into loading its remote images (`load-remote-content-button`).
    /// Flips the manager-owned reveal set and re-emits, so the next `threadDetail`
    /// projects `RemoteImage.revealed: true` for it (render-model.md § D3) — the web
    /// app no longer keeps a `revealedRemote` dictionary. In-memory only.
    #[wasm_bindgen(js_name = revealRemoteImages)]
    pub fn reveal_remote_images(&self, message_id: String) {
        self.manager.reveal_remote_images(MessageId(message_id));
    }

    // ── Selection + existing-thread compose (snapshot-only mutators) ────
    //
    // Thin sync wrappers over the shared manager; the Svelte page re-reads
    // `snapshot()` / `threadDetail()` after each call (observer-driven off the
    // snapshot, no client-side state machine — `conversations.md` §
    // Architectural rules #1).

    /// Select a thread (drives `conversation-item[i]` → detail pane).
    #[wasm_bindgen(js_name = selectThread)]
    pub fn select_thread(&self, thread_id: String) {
        self.manager.select_thread(ThreadId(thread_id));
    }

    /// Clear the selection (back to the empty detail-pane hint).
    #[wasm_bindgen(js_name = clearSelection)]
    pub fn clear_selection(&self) {
        self.manager.clear_selection();
    }

    /// Edit the body draft on an existing thread's compose bar (`dm-text-field`).
    #[wasm_bindgen(js_name = setComposeBody)]
    pub fn set_compose_body(&self, thread_id: String, body: String) {
        self.manager.set_compose_body(ThreadId(thread_id), body);
    }

    /// Reveal / hide the subject input on an existing thread (`topic-toggle-button`).
    #[wasm_bindgen(js_name = toggleTopic)]
    pub fn toggle_topic(&self, thread_id: String) {
        self.manager.toggle_topic(ThreadId(thread_id));
    }

    /// Edit the subject draft on an existing thread (`subject-input`).
    #[wasm_bindgen(js_name = setComposeSubject)]
    pub fn set_compose_subject(&self, thread_id: String, subject: String) {
        self.manager
            .set_compose_subject(ThreadId(thread_id), subject);
    }

    /// Pre-fill / clear reply context on an existing thread (`dm-reply-button[i]`
    /// / `dm-reply-cancel`). `None` clears.
    #[wasm_bindgen(js_name = setReplyTo)]
    pub fn set_reply_to(&self, thread_id: String, message_id: Option<String>) {
        self.manager
            .set_reply_to(ThreadId(thread_id), message_id.map(MessageId));
    }

    /// Seed a reply draft (`dm-reply-button` → `reply_all = false`, sender-only;
    /// `dm-reply-all-button` → `reply_all = true`, every participant but self).
    /// Sets `compose.reply_to` and, on rails with `supports_recipient_selection`
    /// (mail), seeds the editable To line `compose.reply_recipients`
    /// (`conversations.md` § Participants vs reply recipients).
    #[wasm_bindgen(js_name = startReply)]
    pub fn start_reply(&self, thread_id: String, message_id: String, reply_all: bool) {
        self.manager
            .start_reply(ThreadId(thread_id), MessageId(message_id), reply_all);
    }

    /// Add a recipient to the editable reply To line (`dm-reply-recipient-add`).
    /// The raw string is format-parsed (`try_parse_typed_address`); an
    /// unrecognizable string is a no-op. De-duplicated by address identity.
    #[wasm_bindgen(js_name = addReplyRecipient)]
    pub fn add_reply_recipient(&self, thread_id: String, raw: String) {
        if let Some(addr) = try_parse_typed_address(&raw) {
            self.manager.add_reply_recipient(ThreadId(thread_id), addr);
        }
    }

    /// Remove a recipient from the editable reply To line
    /// (`dm-reply-recipient-remove`). Drops it from *this reply only* — thread
    /// history is untouched. The displayed address string round-trips through
    /// `try_parse_typed_address`; `same_address` matches it (mail recipients are
    /// always Email, the only rail with `supports_recipient_selection`).
    #[wasm_bindgen(js_name = removeReplyRecipient)]
    pub fn remove_reply_recipient(&self, thread_id: String, raw: String) {
        if let Some(addr) = try_parse_typed_address(&raw) {
            self.manager
                .remove_reply_recipient(ThreadId(thread_id), addr);
        }
    }

    /// Mark a thread read (`dm-unread-indicator` clears). Stub server-side for now.
    #[wasm_bindgen(js_name = markRead)]
    pub fn mark_read(&self, thread_id: String) {
        self.manager.mark_read(ThreadId(thread_id));
    }

    // ── Add-participant overlay (snapshot-only mutators) ────────────────

    /// Open the add-participant overlay on a thread (`thread-add-participant-button`).
    #[wasm_bindgen(js_name = openAddParticipant)]
    pub fn open_add_participant(&self, thread_id: String) {
        self.manager.open_add_participant(ThreadId(thread_id));
    }

    /// Type into the add-participant picker (`recipient-picker-input` while the
    /// overlay is open).
    #[wasm_bindgen(js_name = setAddParticipantRecipientInput)]
    pub fn set_add_participant_recipient_input(&self, text: String) {
        self.manager.set_add_participant_recipient_input(text);
    }

    /// Close the add-participant overlay, discarding any input.
    #[wasm_bindgen(js_name = cancelAddParticipant)]
    pub fn cancel_add_participant(&self) {
        self.manager.cancel_add_participant();
    }

    /// Commit a resolved Fauna actor (`handle` + 64-hex `actor_id`) as an
    /// add-participant chip. The boundary form of `accept_add_participant_chip`
    /// for a typed-Fauna recipient (the GUI's resolve-then-`acceptCurrentRecipientChip`
    /// path covers the email/handle-probe case); also the e2e add-participant
    /// injection seam (linux `e2e_add` twin).
    #[wasm_bindgen(js_name = acceptAddParticipantFaunaChip)]
    pub fn accept_add_participant_fauna_chip(
        &self,
        handle: String,
        actor_id_hex: String,
    ) -> Result<(), JsValue> {
        let actor_id = parse_actor_hex(&actor_id_hex)?;
        self.manager
            .accept_add_participant_chip(TypedAddress::Fauna { handle, actor_id });
        Ok(())
    }
}

// ── e2e test-helper surface ─────────────────────────────────────────────────
//
// The browser twin of the manager's `#[cfg(any(test, debug_assertions, feature =
// "test-helpers"))]` surface. These let the cross-app snapshot tests
// (`tests/e2e-unified/tests/test_{keying,subject_divider,capability_gating,
// thread_membership,thread_rename}.py`) inject inbound + bootstrap groups
// without standing up real wire backends. Mirrors linux
// `main.rs::handle_conversations_{inject_inbound,create_mls_group}` +
// `conv_backend.rs::disable_e2e_real_backend`.
//
// ⚠ Gated on THIS CRATE'S `test-helpers` feature, which is off by default — so a
// production `just wasm` / `just web` build exports none of it. Until 2026-07-30
// these were plain ungated `#[wasm_bindgen]` exports and this comment claimed they
// were "inert in production": they were not inert, they were *callable* from any
// page script in the shipped bundle (`installMockBackendsForTest` replaces every
// rail backend; `injectSendFailure` fakes send state). `debug_assertions` is NOT
// the lever here — a production wasm build is a RELEASE build — and per
// testing.md § convention 15 rule (b) a wasm export of a seam stays keyed on the
// FEATURE ALONE, never the profile, so the generated JS/.d.ts face is a pure
// function of the feature set. `just web-test` builds the `wasm-core-test` flavor
// with the feature on; the SPA reaches these through a flavor-agnostic dispatch
// (`$lib/conversations`'s `seam()`), so both flavors type-check.
//
// A SEPARATE impl block, deliberately: every method inside it is a test seam, so
// there is no way to add an ungated one by accident.
#[cfg(feature = "test-helpers")]
#[wasm_bindgen]
impl WasmConversationsManager {
    /// Register a `MockRailBackend` for every rail (overwriting any production
    /// backends on this manager), so `injectInboundFromTestJson` can bucket inbound
    /// on any rail — including Bluesky/Nostr/ActivityPub, which have no production
    /// backend wired. The dispatcher calls this once before the first inject
    /// command (the web equivalent of linux's e2e-mode `host::manager()`
    /// install). Idempotent — a repeat re-registers the same mocks.
    #[wasm_bindgen(js_name = installMockBackendsForTest)]
    pub fn install_mock_backends_for_test(&self) {
        self.manager.install_mock_backends_for_test();
    }

    /// Re-register the **real** `FaunaMlsBackend` (stashed in `fauna_mls` at
    /// `with_conversations` construction), overwriting whatever the FaunaMls slot
    /// currently holds — the e2e opt-in into the real wire for the tier_3
    /// `test_fauna_mls_real_roundtrip`. The browser twin of linux
    /// `conv_backend.rs::request_e2e_activation`: where linux must build + register
    /// the real backend lazily (its deps land at AuthSuccess), the web manager
    /// already constructed it, so the opt-in just flips the manager's FaunaMls
    /// entry back from the mock (`installMockBackendsForTest` overwrote it). The
    /// other rails' mock backends may linger harmlessly — `probe_address` tries
    /// FaunaMls first, and the real-wire test's recipients are all 64-hex Fauna
    /// actors. No-op (returns `false`) on a manager built without the real backend
    /// (the client-less `new` path); `true` once re-registered.
    #[wasm_bindgen(js_name = reinstallRealFaunaMlsForTest)]
    pub fn reinstall_real_fauna_mls_for_test(&self) -> bool {
        match self.fauna_mls.borrow().clone() {
            Some(backend) => {
                self.manager.register_backend(backend);
                true
            }
            None => false,
        }
    }

    /// Inject through the SHARED payload parser — the browser twin of the native
    /// `inject_inbound_from_test_json` face. The SPA hands the driver's
    /// `conversations_inject_inbound` payload over whole — `recipients`, the
    /// mail rail's real-self resolution, attachments, labels, `is_own` and
    /// `force_subject_change` included — so no key is re-parsed in the SPA. It
    /// replaced four positional faces whose hand-mapping dropped `recipients`.
    #[wasm_bindgen(js_name = injectInboundFromTestJson)]
    pub fn inject_inbound_from_test_json(&self, payload_json: String) -> Result<(), JsValue> {
        self.manager
            .inject_inbound_from_test_json(payload_json)
            .map_err(|e| JsValue::from_str(&format!("inject inbound: {e}")))
    }

    /// Drop the cached bytes of every attachment named `filename` in thread
    /// `threadId` the way the store's budget eviction does — the browser twin of
    /// the shared `evict_thread_attachments_for_test` tui and linux call. Returns
    /// how many handles were evicted, so the agent refuses a no-op rather than
    /// ack a render that never lost its bytes (`conversations.md` § Attachments →
    /// *Retention*).
    #[wasm_bindgen(js_name = evictThreadAttachmentsForTest)]
    pub fn evict_thread_attachments_for_test(&self, thread_id: String, filename: String) -> u32 {
        self.manager
            .evict_thread_attachments_for_test(ThreadId(thread_id), filename)
    }

    /// Test-only: stamp a pre-resolved link-preview (`PreviewState::Resolved`) for `url` so an
    /// injected bubble's `RenderBlock::LinkPreview` block folds `Resolved` and the web bubble
    /// paints the `link-preview-card` — the browser twin of linux
    /// `handle_conversations_seed_resolved_link_preview` and the conversations counterpart of the
    /// feed's `link_preview` inject spec (render-model.md § D4). A SEPARATE additive method (NOT a
    /// grown inject payload): the resolve is mocked here exactly as the feed link-preview
    /// e2e mocks it (tier_2), since a real resolve needs a live nest OpenGraph fetch. `imageHash`
    /// is `None` when JS passes `undefined`.
    #[wasm_bindgen(js_name = seedResolvedLinkPreviewForTest)]
    pub fn seed_resolved_link_preview_for_test(
        &self,
        url: String,
        title: String,
        description: String,
        image_hash: Option<String>,
    ) {
        self.manager
            .seed_resolved_link_preview_for_test(url, title, description, image_hash);
    }

    /// Test-only: stamp `ComposeState.send_state = Failed { reason }` on
    /// `thread_id` and select it — the same observable state a backend send
    /// error leaves — so the page surfaces it on `error-message` (per
    /// `conversations.md` § Errors & edge cases). The browser twin of linux
    /// `main.rs::handle_conversations_inject_send_failure`. Inert in production
    /// (only the `__fauna_callCommand` e2e hook calls it).
    #[wasm_bindgen(js_name = injectSendFailure)]
    pub fn inject_send_failure(&self, thread_id: String, reason: String) {
        self.manager
            .inject_send_failure_for_test(&ThreadId(thread_id), reason);
    }

    /// Test-only: stamp `ConversationsSnapshot.error` — the exact observable
    /// state a failed **membership/label** wire op leaves
    /// (`confirm_add_participant` / `remove_participant` / `rename_thread`) —
    /// so the page surfaces it on `error-message` (`conversations.md` §
    /// Errors & edge cases). The membership twin of [`Self::inject_send_failure`],
    /// same shape: `key` is an i18n key, `message` its `{message}`
    /// substitution. Inert in production (only the `__fauna_callCommand` e2e
    /// hook calls it).
    #[wasm_bindgen(js_name = injectPageError)]
    pub fn inject_page_error(&self, key: String, message: String) {
        self.manager
            .inject_page_error_for_test(fauna_core::localized::LocalizedText::key_arg(
                key, "message", message,
            ));
    }

    /// The page error the last membership/label gesture stamped, as a plain
    /// `key: message` diagnostic, or `undefined` — for the e2e agent, not for
    /// paint. [`Self::confirm_add_participant`], [`Self::remove_participant`] and
    /// [`Self::rename_thread`] resolve normally even when their wire op failed,
    /// so an agent arm awaiting one reads this back or acks green for an op the
    /// nest refused (`e2e-conventions.md` § convention 11). The wasm door onto
    /// [`ConversationsManager::page_error_diagnostic`], which tui, linux and
    /// windows read the same way.
    #[wasm_bindgen(js_name = pageErrorDiagnosticForTest)]
    pub fn page_error_diagnostic_for_test(&self) -> Option<String> {
        self.manager.page_error_diagnostic()
    }

    /// Create an MLS group thread directly from Fauna `handle`s — the browser
    /// twin of linux `handle_conversations_create_mls_group`. Returns the new
    /// thread id.
    ///
    /// ⚠ Each participant's actor id is derived from its handle, NOT the
    /// `ActorId([0u8; 32])` this seam minted until 2026-08-10: zeros gave every
    /// member of a fixture group ONE identity, so any per-person assertion (a
    /// review flag, a per-member badge) lit up on all of them or on none, and
    /// could not fail for its own reason. The shared helper is deterministic per
    /// handle, so a driver can address a specific member.
    #[wasm_bindgen(js_name = createMlsGroupForTest)]
    pub fn create_mls_group_for_test(&self, participants: Vec<String>) -> String {
        let addrs: Vec<TypedAddress> = participants
            .into_iter()
            .map(|s| TypedAddress::Fauna {
                actor_id: fauna_conversations::manager::test_actor_id_for_handle(&s),
                handle: s,
            })
            .collect();
        self.manager.create_mls_group(addrs).0
    }

    /// Wipe thread/draft/selection/search/sort state (preserving backends +
    /// observers) so each e2e test starts empty — the browser twin of the
    /// manager's `clear_for_test`.
    #[wasm_bindgen(js_name = clearForTest)]
    pub fn clear_for_test(&self) {
        self.manager.clear_for_test();
    }

    /// Arrange a WebDAV-served, content-keyed folder for this owner — the
    /// fixture step `tests/e2e-unified/helpers/webdav_roundtrip.py`'s
    /// `serve_enable_folder` runs before a real WebDAV client PUTs/GETs. The web
    /// twin of tui's `SettingsState::serve_enable_folder_for_test`, and the same
    /// shape: create the Sync set when `create` (the nest's own
    /// `fauna.folders.create`), then run the production serve orchestration
    /// (`FoldersAuthor::serve_set`, the one `foldersServeSet` and the
    /// `folder-webdav-toggle` gesture run). Resolves to the number of served sets
    /// the re-provisioned `WebdavKeysBlob` carries.
    #[wasm_bindgen(js_name = foldersServeEnableForTest)]
    pub fn folders_serve_enable_for_test(
        &self,
        owner_secret_hex: String,
        name: String,
        create: bool,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            if create {
                author
                    .create_set(fauna_client_folders::folders::FolderCreateRequest {
                        name: name.clone(),
                        ..Default::default()
                    })
                    .await
                    .map_err(|e| JsValue::from_str(&format!("create set {name:?}: {e}")))?;
            }
            let count = author
                .serve_set(&name, None, true)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::from_f64(count as f64))
        })
    }
}

impl Default for WasmConversationsManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Full-rail construction + the async wire drivers. Wasm-only because the real
/// `NestOutboundMailSink` / `WsConversationsRpc` (over the `Rc`-based `WsRpcClient`)
/// and the `MlsEngine` live behind `cfg(wasm32)` deps. Built via
/// `WsRpcClient::conversationsManager(selfAddress, selfSecret)` (see `rpc.rs`).
#[cfg(target_arch = "wasm32")]
impl WasmConversationsManager {
    /// Build the unified conversations manager for the logged-in actor: register
    /// **both** rails — the `Rail::Smtp` backend (send + receive over
    /// `EmailClient<WsRpcClient>`) and the `Rail::FaunaMls` backend (E2E MLS DMs
    /// over the `WsConversationsRpc` seam, driving an in-memory `MlsEngine`). The
    /// browser twin of linux `conv_backend.rs::activate` — one page, every rail
    /// (`conversations.md` § Goal). `self_address` is the logged-in
    /// `<handle>@<domain>` (the outbound From + the FaunaMls self-handle, which
    /// derives `self_domain`); `self_secret` is the 32-byte Ed25519 actor secret
    /// the MLS engine builds its credential + signer from (`ActorKeypair::from_secret`).
    ///
    /// MLS state is **in-memory** (`new_in_memory`): the browser has no SQLite,
    /// and the at-rest backup-of-record is the nest's per-channel sealed segment
    /// store regardless (`conversations.md` § Persistence). Group state is rebuilt
    /// from the nest on each session via Welcome-join + channel replay.
    pub fn with_conversations(
        client: fauna_rpc_wasm::WsRpcClient,
        self_address: String,
        self_secret: Vec<u8>,
    ) -> Result<Self, JsValue> {
        let arr: [u8; 32] = self_secret
            .try_into()
            .map_err(|_| JsValue::from_str("self secret must be 32 bytes"))?;
        let keypair = ActorKeypair::from_secret(arr);
        let self_actor = keypair.actor_id();
        // Build the conversations-rail `DraftsSync` over the SAME transport +
        // identity *before* the keypair is moved into the MLS engine. The wrapped
        // `DraftsClient` derives the at-rest `BackupKey` from the seed internally
        // and keeps only that (drafts are owner-only — no signing), so we hand it
        // a borrow here.
        let drafts_sync = Rc::new(DraftsSync::new(
            client.clone(),
            &keypair,
            fauna_protocol::drafts::RAIL_CONVERSATIONS,
        ));
        // Cross-device MLS state sync (`devices.md` § Cross-device MLS group-state
        // sync) over the SAME transport + identity — built *before* the keypair moves
        // into the MLS engine, exactly as `drafts_sync` above is (the wrapped
        // replica client derives the at-rest `BackupKey` from the seed internally and
        // keeps only that, so a borrow suffices). The plane is wired later, on
        // `restoreMlsState` (design §5 restore-before-first-poll); the ctor is
        // non-async (only `load()` touches the wire).
        // ⚠ **Leg 3 of the succession aftermath — resolved HERE, before the
        // plane is built, and passed in rather than fetched later.** The `__mls`
        // re-seal is a *barrier* inside the replica's own `load()`, not a hook
        // racing it (`MlsStateSync::with_predecessors`): a restore that read the
        // still-predecessor-sealed replica would classify the unseal failure as
        // `RestoreRetryEnd::Failed` — **permanent** — and leave a successor's
        // conversations dark for the whole session rather than until the next
        // retry. tui orders it the same way, for the same reason.
        //
        // The resolution is the shared registry walk
        // (`AccountRegistry::predecessor_backup_keys`), never open-coded: the
        // `__mls` and chunk-corpus legs all want this exact list,
        // and a per-app copy of a filter whose failure mode is silent — a
        // dropped row reads as "no key opens it" forever — is precisely the
        // divergence priority #2 exists to prevent.
        //
        // Empty for every identity that never succeeded, which is what makes
        // this free for them: `load()` skips the pass without a round trip.
        let mls_predecessors =
            crate::succession::account_registry().predecessor_backup_keys(&keypair.actor_id_hex());
        let mls_sync = Arc::new(
            MlsStateSync::new(
                Box::new(WsMlsReplicaTransport::new(client.clone())),
                &keypair,
            )
            .with_predecessors(mls_predecessors)
            // Leg 3's NARRATION, the other half of the line above. `with_predecessors` makes the re-seal *happen*;
            // without this the pass runs and reports to nobody, so the Recovery
            // kit section renders no leg-3 line and the journey asserting it
            // reads ''. Capture-free by construction — see
            // `crate::succession::MLS_RESEAL_SINK` for why it has to be.
            .with_reseal_sink(crate::succession::mls_reseal_sink()),
        );
        let engine = Arc::new(
            MlsEngine::new_in_memory(keypair)
                .map_err(|e| JsValue::from_str(&format!("mls engine: {e}")))?,
        );

        let manager = ConversationsManager::new();
        // ONE live cell for both rails (`conversations.md` § State & data shape
        // → *Self-address: live, never baked*) — `setSelfAddress` heals them
        // together, so a manager built before handle/domain resolve (the
        // split-construction shape: MLS/DM delivery must not wait on identity)
        // starts sending the moment the SPA's identity store lands the address.
        let self_address = SelfAddress::new(self_address);
        // SMTP rail — send + receive over the shared email client.
        manager.register_backend(Arc::new(SmtpBackend::new_shared(
            Arc::new(NestOutboundMailSink::new(client.clone())),
            Arc::clone(&self_address),
        )));
        // FaunaMls rail — E2E MLS DMs over the WS-RPC conversations seam. Keep a
        // clone of the client for the durable-inbox `drain` (the `WsRpcClient`
        // itself is the `RpcRequester` an `InboxClient` wraps — `drainInbox`).
        let rpc = Arc::new(WsConversationsRpc::new(client.clone()));
        // Wire the home-nest link-preview seam (render-model.md § D4) with the SAME object
        // (`WsConversationsRpc` impls both `ConversationsRpc` and `LinkPreviewRpc`), so a
        // conversation bubble's bare-url `LinkPreview` resolves through the manager.
        manager.set_link_preview_rpc(rpc.clone());
        // The room plane's four nest-backed seams, the SAME object again
        // (`WsConversationsRpc` implements all four), registered on the backend
        // below as one bundle so no seam can be left out.
        let room_seams = RoomSeams::from_rpc(&rpc);
        let fauna_mls = Arc::new(FaunaMlsBackend::new_shared(
            engine,
            rpc,
            Arc::clone(&self_address),
            self_actor,
        ));
        // The `mls_sync` plane above exists, so no key-package mint may publish
        // until `restoreMlsState` wires its save-before-publish seam (or gives up).
        fauna_mls.expect_replica_restore();
        // Member content-key custody-ingest (Phase 0 — the read leg;
        // `folders.md` § Sharing): the same seam every native app wires
        // via `set_folder_custody_sink` (`fauna-ffi`'s `nest_client.rs`,
        // linux/tui's own conv_backend.rs) — the read twin of the owner-side
        // custody writer, now wasm-generic
        // (`fauna_client_folders::custody_ingest`). Without it a member lists
        // a shared set but can never decrypt its bytes, AND — the specific gap
        // this closes — `join_folder_welcome` never records a cross-nest
        // `ForeignFolder` at all (`fauna_mls.rs`'s `record_foreign_set` call
        // is itself gated on a sink being registered), so a foreign share
        // silently never appeared in the list. No
        // observer: web has no local sync engine to poke on rotation (the
        // trait's own documented default).
        // `arr` is `Copy`, so this keypair is independent of the one already
        // moved into the MLS engine above.
        fauna_mls.set_folder_custody_sink(Arc::new(
            fauna_client_folders::NestFolderCustodySink::new(
                client.clone(),
                crate::account_runtime::folder_key_store(
                    ActorKeypair::from_secret(arr).actor_id_hex(),
                ),
            ),
        ));
        // The room plane's four nest-backed seams (`RoomSeams`):
        // - the floor-roster REPORT (`conversation-rooms.md` § The floor
        //   roster): after every membership commit this device authors on a
        //   governed room the backend reports the resulting roster to the
        //   room's home nest, which stores it as the floor roster the custody
        //   serve door reads. Unset, every owed report is tallied and dropped;
        // - the floor-roster READ, `fauna.conversations.room.list_roster`'s
        //   client half, which answers the handle of an actor this device has
        //   never met. Driven after each channel's walk releases its lock in
        //   `poll_conversations`, never inside it: it awaits a nest round trip
        //   and nothing downstream waits on its answer;
        // - the community class's GENERATION read (§ The three classes →
        //   *Community*), which fetches the X-Wing wrap of a room's generation
        //   key;
        // - the community class's CEREMONY: founding, joining, keying and the
        //   governance doors, which the room-settings Save routes a community
        //   room to.
        // The generation read's other half, the group-reception keys, rests on
        // the account plane: it is registered at the account runtime's
        // store-ready edge (`WsRpcClient::startAccountRuntime` →
        // `crate::account_runtime`), which resolves after this constructor.
        // Until then a community room's records stay unopened here, and
        // founding or accepting an invitation is refused by name rather than
        // keyed to a secret this browser could not keep.
        fauna_mls.set_room_seams(room_seams);
        manager.register_backend(fauna_mls.clone());
        // The bridged rail — one backend for every bridge serving the account
        // (`conversations.md` § Where logic lives → *The `Bridged` adapter*),
        // over the SAME glue the native apps register
        // (`fauna_client_conversations::NestBridgedGlue`): it seals to the
        // bridge's key and to the account's own, and opens the inbox under the
        // mail key set this manager already holds for the SMTP rail. Registered
        // on the raw manager, because this app has no session; the JS receive
        // loop drives [`Self::poll_bridged`].
        let standing_keypairs: Rc<RefCell<Vec<_>>> = Default::default();
        let recipient_public: Rc<RefCell<Option<_>>> = Default::default();
        let mail_epoch_roots: Rc<RefCell<Vec<_>>> = Default::default();
        let bridged_glue = fauna_client_conversations::NestBridgedGlue::new(
            client.clone(),
            Arc::new(WebBridgedKeySource {
                standing: Rc::clone(&standing_keypairs),
                epoch_roots: Rc::clone(&mail_epoch_roots),
                own_public: Rc::clone(&recipient_public),
            }),
        );
        let bridged_backend = Arc::new(BridgedBackend::new(bridged_glue.clone()));
        manager.register_backend(bridged_backend.clone());
        let bridged_glue: Arc<dyn BridgedSource> = bridged_glue;
        let local_detections = install_local_detection_store(&manager);

        // ── The in-group succession witness (`succession-propagation.md`
        // § Propagation → *MLS groups*) ──────────────────────────────────────
        //
        // Without it every `GroupMetaMessage::Succession` degrades to the bare
        // add: a member sees "someone added a stranger" where a peer actually
        // recovered their account, and the participant row keeps naming the
        // retired identity forever. web was the last app in that state.
        //
        // Registered on the raw backend rather than through
        // `ConversationsSession::set_succession_witness`, because this app has
        // no session — its receive loop is JS-owned (see `poll_conversations`).
        // Everything but the dialer is the shared policy the six native apps
        // run; see `crate::succession_witness`.
        //
        // The anchors' durable store (`fauna.state.peer-anchors`) is lent late,
        // through the manager, by the shared store-ready registration
        // (`crate::account_runtime`'s `conversation_seams::wire_parts`) — the
        // same one the native apps run, so nothing to hand in here and nothing
        // to forget. Until it is lent the anchors read as unreadable, never
        // as empty.
        let witness = Arc::new(fauna_client_recovery::ChainWitness::new(
            fauna_client_recovery::ThreadParticipantAnchors::new(
                // `Weak`, never the manager itself: the witness is parked on
                // the backend this manager owns, so a strong handle here is a
                // cycle that outlives the session (the type's own doc).
                Arc::downgrade(&manager),
            ),
            crate::succession_witness::WebSuccessionChainSource,
        ));
        fauna_mls.set_succession_witness(
            Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
        );
        // This session ticks a harvest sweep from `poll_conversations` (the
        // `peer_anchor_sweep` state built below), so the witness may hold a
        // held-head verdict until that sweep has settled the peer
        // (`identity-succession.md` § The succession statement → *the harvest
        // wait*). Native arms from the receive loop's prologue; this app has
        // no such loop, and here is before any poll can run. The backend
        // learns it too: the folder commit walk's hold behind a parked
        // succession statement waits only where a sweep will end the wait.
        witness.arm_harvest_wait();
        fauna_mls.note_harvest_sweep_armed();
        let succession_witness = Some(witness);
        // The producer half — the peer-anchor harvest's log. The sweep itself
        // is ticked from `poll_conversations`, this app's own clock; what is
        // built here is only the report both halves of the state contract read.
        let peer_anchor_harvest: Arc<fauna_client_recovery::harvest::HarvestLog> =
            Default::default();

        Ok(Self {
            manager,
            self_address,
            standing_keypairs,
            recipient_public,
            mail_epoch_roots,
            bridged: RefCell::new(Some((bridged_backend, bridged_glue))),
            bridged_cursor: Rc::new(RefCell::new(Some((0, HashSet::new())))),
            seen: RefCell::new(HashSet::new()),
            notif_tracker: MessageNotificationTracker::new(),
            spam_scorer: RefCell::new(None),
            local_detections,
            succession_witness: RefCell::new(succession_witness),
            peer_anchor_harvest,
            peer_anchor_sweep: Rc::new(RefCell::new(Some(
                fauna_client_recovery::harvest::PeerAnchorSweepState::new(),
            ))),
            fauna_mls: RefCell::new(Some(fauna_mls)),
            conv_cursors: Rc::new(RefCell::new(HashMap::new())),
            folder_cursors: Rc::new(RefCell::new(HashMap::new())),
            scheduling_cursors: Rc::new(RefCell::new(HashMap::new())),
            scheduling_successions: fauna_client_caldav::SuccessionMemo::new(),
            nest_client: RefCell::new(Some(client)),
            drafts_sync: Some(drafts_sync),
            mls_sync: Some(mls_sync),
        })
    }
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl WasmConversationsManager {
    /// The moderation queue's **local half**: this session's post-decrypt local
    /// detections as a JS array of the shared `LocalDetection` shape
    /// (`{ content_id, content_type, category, confidence_per_mille (u16 0–1000),
    /// timestamp (i64 µs-epoch) }`) — feed it straight to the `moderationQueue`
    /// free fn alongside the server `fauna.moderation.actions` rows to get the
    /// merged queue. The browser twin of native's
    /// `ConversationsSession::moderation_local_detections`. Empty until the receive
    /// loop classifies an incoming spam message post-decrypt; the decrypted body
    /// itself never crosses into JS (`moderation.md` § State & data shape — the
    /// detections are held client-side, never round-tripped through the nest).
    #[wasm_bindgen(js_name = moderationLocalDetections)]
    pub fn moderation_local_detections(&self) -> Result<JsValue, JsValue> {
        crate::rpc::to_js(&self.local_detections.lock().unwrap().snapshot())
    }

    /// Drop the local-detection row for `content_id` after the user trains a
    /// correction on it (`train-correction-button`) — the row is corrected, so it
    /// leaves the queue. `false` for a server row (not in this store) or one already
    /// gone. Twin of native's `moderation_remove_local_detection`.
    #[wasm_bindgen(js_name = moderationRemoveLocalDetection)]
    pub fn moderation_remove_local_detection(&self, content_id: String) -> bool {
        self.local_detections.lock().unwrap().remove(&content_id)
    }

    /// The retained decrypted plaintext body of one message, by the message-id
    /// string a moderation `QueueRow.content_id` carries for a `source: "Local"`
    /// row. The train-correction surface (`train-correction-button`) reads this to
    /// feed the ham correction to the client-side tier-1 spam-model write
    /// (`trainSpamModelClient`) with the same post-decrypt text the classifier
    /// saw — text only the client holds (MLS-sealed at rest,
    /// `moderation.md` § Layout & flow). `undefined` once the message has aged out
    /// of the thread store (the correction then just clears the flag). Wasm twin
    /// of native's `ConversationsManager::message_body`.
    #[wasm_bindgen(js_name = messageBody)]
    pub fn message_body(&self, message_id: String) -> Option<String> {
        self.manager.message_body(&message_id)
    }

    /// Resolve the current recipient input to a rail/chip — drives
    /// `recipient-resolve-status`. For an email this is a local shape-parse
    /// (`Rail::Smtp`), no nest round-trip; awaited only for API uniformity with
    /// the FaunaMls probe. Resolves the JS Promise once the snapshot is updated.
    #[wasm_bindgen(js_name = resolveRecipient)]
    pub fn resolve_recipient(&self) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.resolve_recipient().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Resolve link-preview metadata (render-model.md § D4) for a bare-url `LinkPreview`
    /// block in a conversation bubble — the conversations twin of
    /// `WasmFeedManager.resolveLinkPreview`. Fire-once per URL (idempotent in the manager);
    /// the conversations page calls it on each `Resolving` bubble preview and re-renders the
    /// thread on resolve. Resolves to `undefined` (the manager folds the terminal state into
    /// the next `thread_detail`).
    #[wasm_bindgen(js_name = resolveLinkPreview)]
    pub fn resolve_link_preview(&self, url: String) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.resolve_link_preview(url).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Materialize the new-thread compose into a thread and send it
    /// (`manager.send_new_thread()` → `SmtpBackend::send` → `NestOutboundMailSink::submit`
    /// → `fauna.email.send`). Resolves to the new thread id (or `null` if there
    /// was nothing to send); rejects with the backend error string on failure.
    #[wasm_bindgen(js_name = sendNewThread)]
    pub fn send_new_thread(&self) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            match mgr.send_new_thread().await {
                Ok(Some(id)) => Ok(JsValue::from_str(&id.0)),
                Ok(None) => Ok(JsValue::NULL),
                Err(e) => Err(JsValue::from_str(&format!("{e}"))),
            }
        })
    }

    /// Send the composed body on an **existing** thread (`dm-send-button`):
    /// `manager.send` → the resolved rail backend (`SmtpBackend::send` /
    /// `FaunaMlsBackend::send`). A forked-but-unbound FaunaMls group bootstraps
    /// lazily here. Resolves to `undefined`; rejects with the backend error.
    #[wasm_bindgen(js_name = send)]
    pub fn send(&self, thread_id: String) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.send(ThreadId(thread_id))
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&format!("{e}")))
        })
    }

    /// Confirm the open add-participant overlay: on a `FaunaMls` 1:1 this forks a
    /// fresh MLS group (posts Commit + Welcome) and selects it; on every other
    /// `(rail, flavor)` it adds in place. Resolves to the (possibly new) thread id
    /// or `null`.
    #[wasm_bindgen(js_name = confirmAddParticipant)]
    pub fn confirm_add_participant(&self) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            match mgr.confirm_add_participant().await {
                Some(id) => Ok(JsValue::from_str(&id.0)),
                None => Ok(JsValue::NULL),
            }
        })
    }

    /// Rename a thread (`thread-rename-button`; enabled iff
    /// `capabilities.supports_rename`). On a bound FaunaMls group this posts the
    /// encrypted `GroupMeta::NameChanged` envelope. Resolves to `undefined`.
    #[wasm_bindgen(js_name = renameThread)]
    pub fn rename_thread(&self, thread_id: String, new_label: String) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.rename_thread(ThreadId(thread_id), new_label).await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Remove a Fauna participant (`handle` + 64-hex `actor_id`) from a bound
    /// FaunaMls group (posts the MLS Commit; no Welcome). **Both halves key on
    /// `actor_id`** — the wire op finds the MLS leaf by it, and the snapshot
    /// removal matches by it too (`TypedAddress::same_participant`), so a
    /// caller passing a stale or colliding `handle` still removes exactly the
    /// identity it named. The snapshot half used to key on the handle, which
    /// took same-handle bystanders with it.
    #[wasm_bindgen(js_name = removeParticipant)]
    pub fn remove_participant(
        &self,
        thread_id: String,
        handle: String,
        actor_id_hex: String,
    ) -> js_sys::Promise {
        let actor_id = match parse_actor_hex(&actor_id_hex) {
            Ok(a) => a,
            Err(e) => return js_sys::Promise::reject(&e),
        };
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.remove_participant(
                ThreadId(thread_id),
                TypedAddress::Fauna { handle, actor_id },
            )
            .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── The room policy editor's Save (`room_settings` sub-page) ─────────
    //
    // `ui/conversations.md` § Element IDs: `room-settings-save-button`
    // "commits every staged change as its own policy commit, the hand-over
    // last, and closes only when all landed". The whole loop — the order, the
    // stop-at-first-refusal, the verdict — is
    // `ConversationsManager::apply_room_settings`, shared by all seven apps,
    // so web's Save is ONE call like every other leg's and this wrapper adds
    // no policy of its own (`conversation-rooms.md` § Implementation status
    // today; priority #2).

    /// Commit the editor's staged changes. `edits` is the
    /// [`RoomSettingsEdit`] list [`roomSettingsEdits`] produced, handed across
    /// as a `JsValue` — never re-derived in TypeScript. Resolves to `true`
    /// only when every edit landed, which is the editor's close condition; a
    /// `false` leaves the page's `error-message` painted by whichever call
    /// refused.
    ///
    /// [`RoomSettingsEdit`]: fauna_conversations::room_settings::RoomSettingsEdit
    /// [`roomSettingsEdits`]: Self::room_settings_edits
    #[wasm_bindgen(js_name = applyRoomSettings)]
    pub fn apply_room_settings(
        &self,
        thread_id: String,
        edits: JsValue,
    ) -> Result<js_sys::Promise, JsValue> {
        let edits: Vec<RoomSettingsEdit> = crate::rpc::from_js(edits)?;
        let mgr = self.manager.clone();
        Ok(future_to_promise(async move {
            let all_landed = mgr.apply_room_settings(ThreadId(thread_id), edits).await;
            Ok(JsValue::from_bool(all_landed))
        }))
    }

    /// Seed the editor off the thread's projected room
    /// ([`RoomSettingsDraft::seed`]) — `null` where the thread models no room
    /// or carries no policy to edit. The draft crosses as a plain JS object;
    /// the SPA holds it opaquely and stages through the calls below, so the
    /// at-most-one-staged hand-over rule and each row's eligibility are
    /// decided in Rust exactly once for all seven apps.
    ///
    /// [`RoomSettingsDraft::seed`]: fauna_conversations::room_settings::RoomSettingsDraft::seed
    #[wasm_bindgen(js_name = roomSettingsSeed)]
    pub fn room_settings_seed(&self, thread_id: String) -> Result<JsValue, JsValue> {
        let detail = self.manager.thread_detail(ThreadId(thread_id));
        match detail.as_ref().and_then(RoomSettingsDraft::seed) {
            Some(draft) => crate::rpc::to_js(&draft),
            None => Ok(JsValue::NULL),
        }
    }

    /// Stage `room-join-rule-select`'s token. Hand in the draft, take back the
    /// staged one — the UniFFI twins' by-value shape, for the same reason.
    #[wasm_bindgen(js_name = roomSettingsSetJoinRule)]
    pub fn room_settings_set_join_rule(
        &self,
        draft: JsValue,
        token: String,
    ) -> Result<JsValue, JsValue> {
        let mut draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        draft.set_join_rule_token(&token);
        crate::rpc::to_js(&draft)
    }

    /// Stage `room-history-policy-select`'s token.
    #[wasm_bindgen(js_name = roomSettingsSetHistoryPolicy)]
    pub fn room_settings_set_history_policy(
        &self,
        draft: JsValue,
        token: String,
    ) -> Result<JsValue, JsValue> {
        let mut draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        draft.set_history_policy_token(&token);
        crate::rpc::to_js(&draft)
    }

    /// Stage `room-admin-toggle[i]` — index-parallel with
    /// `thread-member-chip[i]`, so the caller passes the chip's own index and
    /// never a re-derived member slot.
    #[wasm_bindgen(js_name = roomSettingsToggleAdmin)]
    pub fn room_settings_toggle_admin(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<JsValue, JsValue> {
        let mut draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        draft.toggle_admin(index as usize, &self.room_participants(&thread_id));
        crate::rpc::to_js(&draft)
    }

    /// Stage `room-owner-transfer-button[i]`. At most one hand-over may be
    /// staged at a time; that rule lives in the draft, not here.
    #[wasm_bindgen(js_name = roomSettingsToggleTransfer)]
    pub fn room_settings_toggle_transfer(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<JsValue, JsValue> {
        let mut draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        draft.toggle_transfer(index as usize, &self.room_participants(&thread_id));
        crate::rpc::to_js(&draft)
    }

    /// Whether either control may act on row `index` at all — a Fauna member
    /// who is not the owner. A leg's greying is exactly
    /// `can_appoint_admins && eligible` / `can_transfer_ownership && eligible`,
    /// and **no app re-derives who is eligible**.
    #[wasm_bindgen(js_name = roomSettingsIsEligible)]
    pub fn room_settings_is_eligible(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<bool, JsValue> {
        let draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        Ok(draft.is_eligible(index as usize, &self.room_participants(&thread_id)))
    }

    /// `room-admin-toggle[i]`'s `checked` state, staging included.
    #[wasm_bindgen(js_name = roomSettingsAdminAt)]
    pub fn room_settings_admin_at(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<bool, JsValue> {
        let draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        Ok(draft.admin_at(index as usize, &self.room_participants(&thread_id)))
    }

    /// Whether row `index` is the room's owner — the row neither control is
    /// ever live on.
    #[wasm_bindgen(js_name = roomSettingsIsOwnerAt)]
    pub fn room_settings_is_owner_at(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<bool, JsValue> {
        let draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        Ok(draft.is_owner_at(index as usize, &self.room_participants(&thread_id)))
    }

    /// Whether the hand-over is staged on row `index`.
    #[wasm_bindgen(js_name = roomSettingsTransferStagedAt)]
    pub fn room_settings_transfer_staged_at(
        &self,
        draft: JsValue,
        thread_id: String,
        index: u32,
    ) -> Result<bool, JsValue> {
        let draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        Ok(draft.transfer_staged_at(index as usize, &self.room_participants(&thread_id)))
    }

    /// The draft's diff, in the order Save must issue it — feed it straight to
    /// [`applyRoomSettings`](Self::apply_room_settings).
    #[wasm_bindgen(js_name = roomSettingsEdits)]
    pub fn room_settings_edits(
        &self,
        draft: JsValue,
        thread_id: String,
    ) -> Result<JsValue, JsValue> {
        let draft: RoomSettingsDraft = crate::rpc::from_js(draft)?;
        crate::rpc::to_js(&draft.edits(&self.room_participants(&thread_id)))
    }

    /// The thread's participants, which every staging call above indexes
    /// against. Read here rather than passed in, so a caller cannot hand the
    /// draft a list that disagrees with the chips it is painting.
    fn room_participants(&self, thread_id: &str) -> Vec<TypedAddress> {
        self.manager
            .thread_detail(ThreadId(thread_id.to_string()))
            .map(|d| d.participants)
            .unwrap_or_default()
    }

    /// Replenish the local actor's one-time key-package pool (login top-up) so
    /// peers can fetch one to add us to a group. Best-effort + idempotent (the
    /// count check makes a repeat a no-op). Resolves to the number stored.
    #[wasm_bindgen(js_name = ensureKeypackages)]
    pub fn ensure_keypackages(&self, target: u64) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.ensure_keypackages(target)
                .await
                .map(|n| JsValue::from_f64(n as f64))
                .map_err(|e| JsValue::from_str(&format!("{e}")))
        })
    }

    /// Publish the single reusable last-resort key package (Spec Y2 — keeps us
    /// `addressable` after the one-time pool drains). Idempotent. Resolves to
    /// `undefined`.
    #[wasm_bindgen(js_name = ensureLastResortKeypackage)]
    pub fn ensure_last_resort_keypackage(&self) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.ensure_last_resort_keypackage()
                .await
                .map(|()| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&format!("{e}")))
        })
    }

    // ── FaunaMls inbound (JS-owned loop drives these) ───────────────────
    //
    // The browser owns the timer + push subscriptions (in JS, off the
    // `fauna.conversations.welcome.received` / `.channel.message` push events,
    // like the mail `inbox.fetch` loop). It hands each Welcome to `ingestWelcome`
    // and ticks `pollConversations` per push / backstop interval — the wasm twin
    // of linux `conv_backend.rs`'s select-loop. All MLS crypto stays in
    // `FaunaMlsBackend` (`conversations.md` § Architectural rules #2).

    /// Process a same-nest MLS Welcome (`welcome_bytes` for `channel_id_hex`):
    /// join + bind the group, materialize its thread. Idempotent (a re-delivered
    /// Welcome for an already-bound channel is a no-op). Resolves to the bound
    /// thread id, or `null` when no FaunaMls backend is wired.
    #[wasm_bindgen(js_name = ingestWelcome)]
    pub fn ingest_welcome(
        &self,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
    ) -> js_sys::Promise {
        let Some(backend) = self.fauna_mls.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::NULL);
        };
        let mgr = self.manager.clone();
        future_to_promise(async move {
            // Same-nest (no home nest URL): the web cross-nest mailbox-less rail is
            // a documented follow-on (`direct-messages.md` § Implementation status
            // today — the wasm discovery path is native-gated).
            match ingest_welcome(&backend, &mgr, &channel_id_hex, &welcome_bytes, "").await {
                Ok(thread_id) => Ok(JsValue::from_str(&thread_id.0)),
                Err(e) => Err(JsValue::from_str(&format!("ingest welcome: {e}"))),
            }
        })
    }

    /// Poll every bound FaunaMls channel for new ciphertext (fetch → decode →
    /// MLS-decrypt → ingest), advancing each channel's `seq` cursor. The reconnect
    /// / missed-push backstop the JS loop ticks. Resolves to the total number of
    /// messages ingested this pass.
    #[wasm_bindgen(js_name = pollConversations)]
    pub fn poll_conversations(&self) -> js_sys::Promise {
        let Some(backend) = self.fauna_mls.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::from_f64(0.0));
        };
        let mgr = self.manager.clone();
        let cursors = self.conv_cursors.clone();
        // The peer-anchor harvest's handles, cloned out here so no `RefCell`
        // borrow is held across an await.
        let harvest_client = self.nest_client.borrow().clone();
        let harvest_log = self.peer_anchor_harvest.clone();
        let harvest_sweep = self.peer_anchor_sweep.clone();
        future_to_promise(async move {
            // The wasm twin of the native sweep's first step: a group another of
            // this account's devices joined is imported + bound BEFORE the walk
            // below, so it is polled in this same sweep.
            fauna_conversations::session::adopt_sibling_groups_first(&backend).await;
            let mut total = 0usize;
            // A per-channel failure is non-fatal — the next tick retries the whole
            // sweep — so keep going and remember the first error. Only reject when
            // nothing was ingested *and* something failed; otherwise the partial
            // success is the useful answer (and the JS loop ticks again).
            let mut first_err: Option<String> = None;
            for channel in backend.bound_channels() {
                // Read + copy the cursor (no `conv_cursors` borrow held across the
                // await), poll, then write the advanced cursor back. First encounter
                // seeds from the injected `ChannelCursor` (`resume_seq` — the restored
                // `history/<ch>` watermark on a device-synced tab, else `0`), so a
                // restored browser tab resumes from where this identity last folded
                // rather than re-walking pre-restore history. The wasm twin of native
                // `ConversationsSession::poll_conversations` (session.rs).
                let mut cur = *cursors.borrow_mut().entry(channel).or_insert_with(|| {
                    backend
                        .channel_cursor()
                        .map(|c| c.resume_seq(&channel))
                        .unwrap_or(0)
                });
                // Serialize this channel's inbound drain against a concurrent gated
                // commit (`FaunaMlsBackend::channel_lock`): a gated send stages a
                // pending the engine cannot `process_commit` over. Held across the
                // await; the gate's own catch-up poll runs inside the gated section
                // and never re-takes it.
                let channel_lock = backend.channel_lock(&channel);
                let _guard = channel_lock.lock().await;
                // Past the one-shot key-in stop: the browser builds no local
                // index (`content-index.md` § Where queries run), so there is no
                // catch-up window here to reopen and nothing to gain by
                // stopping (`poll_inbound_conv_past_key_in`).
                let polled =
                    poll_inbound_conv_past_key_in(&backend, &mgr, &channel, &mut cur, 0).await;
                // An ownership offer the walk parked for this identity
                // completes here — after the walk, under the same lock, never
                // inside it (`FaunaMlsBackend::complete_ownership_offer_locked`).
                backend.complete_ownership_offer_locked(&channel).await;
                match polled {
                    Ok(outcome) => {
                        if outcome.stalled && !outcome.awaiting_key {
                            // Stopped before a commit this device never
                            // incorporated (`devices.md` Rule 2). `cur` sits
                            // behind it, so the watermark below stays safe; the
                            // strand stays loud and the next tick retries the heal.
                            //
                            // A community room waiting for its key-in stops the
                            // same way and is NOT a fault, so it stays quiet
                            // (`ConvPollOutcome::awaiting_key`).
                            web_sys::console::error_1(&JsValue::from_str(&format!(
                                "conv poll stalled before an unincorporated commit on {channel} \
                                 — channel is behind the group's epoch until a resync heals it"
                            )));
                        }
                        total += outcome.ingested;
                        cursors.borrow_mut().insert(channel, cur);
                        // Report the folded seq back so the next `history/<ch>` save
                        // snapshots the right watermark (a no-op with no cursor seam).
                        if let Some(c) = backend.channel_cursor() {
                            c.advance(&channel, cur);
                        }
                    }
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(format!("poll_inbound_conv {channel}: {e}"));
                        }
                    }
                }
                // Name any member the walk seated that this device has never
                // met, AFTER releasing the channel lock — `poll_bound`'s own
                // step, in its order, so web's drain path and native's stay
                // behaviourally identical (`conversation-rooms.md` § The floor
                // roster). Each awaits a nest round trip; none of them may
                // hold the lock a gated commit is waiting on.
                drop(_guard);
                backend.tend_community_room(&mgr, &channel).await;
                // The group-ful mirror of it: a room whose best-effort birth
                // report went undelivered has no floor at its home, and a 1:1
                // whose report went undelivered never will (`FaunaMlsBackend::backfill_floor_roster`).
                backend.backfill_floor_roster(&channel).await;
                backend.resolve_nameless_members(&mgr, &channel).await;
            }
            // Attachments the budget evicted and a render has since asked for
            // — fetched again from where they rest, the wasm twin of the
            // native sweep's step (`conversations.md` § Attachments →
            // *Retention*). Web has no in-process poke, so a miss waits for
            // this tick.
            fauna_conversations::backends::fauna_mls::refill_evicted_attachments(&backend, &mgr)
                .await;
            // ── The peer-anchor harvest sweep, on this app's own clock ───
            //
            // The producer half of the member-path anchor
            // (`succession-propagation.md` § Propagation → *MLS groups*, the
            // peer-profile harvest): a Welcome-joined roster row carries no
            // handle, so without this the witness registered in
            // `with_conversations` holds no anchor for exactly the peers the
            // in-group statement is about.
            //
            // **After the walk, never before.** The walk is what materializes
            // the nameless roster rows this sweeps for, and what parks a
            // statement the witness could not yet settle; the pass then seeds
            // and re-drives — so a Welcome and its ceremony's statement
            // arriving in one tick still re-point. Native gets that ordering
            // from a 5 s timer racing its own receive loop; here it holds by
            // construction.
            //
            // The policy — the once-per-peer-per-session guard, the retry
            // ladder, the arm classification, the re-drive — is
            // `PeerAnchorSweepState::run_pass`, shared verbatim with every
            // native app. This block is the clock and nothing else.
            // ⚠ The take MUST be its own statement. Written inside the `if let`
            // scrutinee, the `RefMut` temporary lives until the end of the
            // `if let` — so the put-back below re-entered `borrow_mut()` on a
            // cell this very branch still held, and `pollConversations`
            // panicked (`BorrowMutError`) on EVERY tick of every real web
            // session that has a sweep configured. A wasm panic kills the task
            // and its JS promise never settles, so the whole receive rail died
            // silently: no drain, no Welcome applied, no thread — which is how
            // all four room journeys failed on their first web run
            // (2026-09-12), each on a peer seat that never saw the room.
            // Measured, not reasoned: the browser console's `[pageerror]
            // unreachable` beside this file's line number is the only witness a
            // dead wasm task leaves. The clone-out above says the intent —
            // "no `RefCell` borrow is held across an await" — that the inline
            // take defeated.
            let taken_sweep = harvest_sweep.borrow_mut().take();
            if let (Some(client), Some(mut sweep)) = (harvest_client, taken_sweep) {
                sweep
                    .run_pass(
                        &mgr,
                        &client,
                        &harvest_log,
                        &crate::succession_witness::WebParkedRedrive {
                            backend: Arc::clone(&backend),
                            manager: Arc::clone(&mgr),
                        },
                    )
                    .await;
                *harvest_sweep.borrow_mut() = Some(sweep);
            }
            match first_err {
                Some(msg) if total == 0 => Err(JsValue::from_str(&msg)),
                _ => Ok(JsValue::from_f64(total as f64)),
            }
        })
    }

    /// One read of the bridged rooms and inbox — the wasm twin of native
    /// `ConversationsSession::poll_bridged`, over the same shared
    /// [`poll_inbound_bridged`] driver: the rooms load each bridge's declared
    /// identity and the family gate's marker, then every new inbox row is
    /// opened under the account's mail keys and ingested on the bridged rail.
    /// The JS receive loop calls it on its ticker and on
    /// `fauna.bridges.push.conversation_changed`. Until mail is enabled the
    /// rooms still list and paint and the inbox read is a quiet no-op.
    ///
    /// Resolves to the number of messages ingested; a failure is logged and
    /// resolves `0`, since the next wake retries from the same cursor and this
    /// must never break the receive pass it shares.
    #[wasm_bindgen(js_name = pollBridged)]
    pub fn poll_bridged(&self) -> js_sys::Promise {
        let Some((backend, source)) = self.bridged.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::from_f64(0.0));
        };
        let mgr = self.manager.clone();
        let cursor = self.bridged_cursor.clone();
        future_to_promise(async move {
            // Taken across the awaits; an overlapping pass skips.
            let Some((mut after_id, mut seen)) = cursor.borrow_mut().take() else {
                return Ok(JsValue::from_f64(0.0));
            };
            let polled =
                poll_inbound_bridged(source.as_ref(), &backend, &mgr, &mut after_id, &mut seen, 0)
                    .await;
            *cursor.borrow_mut() = Some((after_id, seen));
            match polled {
                Ok(n) => Ok(JsValue::from_f64(n as f64)),
                Err(e) => {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "poll bridged inbox: {e}"
                    )));
                    Ok(JsValue::from_f64(0.0))
                }
            }
        })
    }

    /// Apply pending membership commits on every **folder** channel this engine
    /// holds — the remaining-members' epoch-advance liveness for shared folders
    /// (5d(d); `mls-group-key-material.md` § Rotate-on-removal, liveness half).
    /// Without it a web member stays at the pre-removal epoch and fails closed on
    /// the owner's re-published content-key envelope.
    ///
    /// Drives the **shared** [`poll_folder_feed`] loop the native receive ticker
    /// runs — folder channels are commit-only and bind no thread, so they are
    /// deliberately outside `pollConversations`'s `bound_channels()` sweep. The JS
    /// receive tick calls this alongside `drainInbox` / `pollConversations`.
    /// Per-channel failure is non-fatal (the next tick retries), so this always
    /// resolves.
    #[wasm_bindgen(js_name = pollFolders)]
    pub fn poll_folders(&self) -> js_sys::Promise {
        let Some(backend) = self.fauna_mls.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let cursors = self.folder_cursors.clone();
        future_to_promise(async move {
            // Copy out, poll, write back — never hold the `RefCell` borrow across
            // an await (the `conv_cursors` discipline above).
            let mut owned = cursors.borrow().clone();
            poll_folder_feed(&backend, &mut owned).await;
            *cursors.borrow_mut() = owned;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Drain every **scheduling** channel this engine has joined — the recipient
    /// half of the mailbox-less CalDAV iMIP rail (`caldav-server.md` § Server-side
    /// auto-schedule, Half-1: "needs a client-side scheduling-inbox drain loop",
    /// since a mailbox-less user runs no mail poll). Each channel's
    /// `ChannelMessageBody::Scheduling` body is MLS-decrypted to the raw RFC 5322
    /// iMIP and applied to this actor's calendar, so an invitation sent from
    /// another calendar lands on the Events page.
    ///
    /// Drives the **shared** [`poll_scheduling_feed`] the native receive ticker
    /// runs, over the [`WebSchedulingSink`] twin of the native
    /// `NestSchedulingSink` — scheduling channels are one-off deliveries that bind
    /// no thread, so like folder channels they sit outside `pollConversations`'s
    /// `bound_channels()` sweep and need their own drive. The JS receive tick calls
    /// this alongside `drainInbox` / `pollConversations` / `pollFolders`.
    ///
    /// `secret_hex` rides in per call rather than being retained (the manager
    /// deliberately holds no identity seed), and
    /// `now_secs` rides in from JS under the wasm-time discipline (no `Date::now()`
    /// in wasm). It is **`f64`, not `i64`** — a wasm `i64` parameter surfaces in JS
    /// as a `bigint`, which every other timestamp seam here avoids.
    ///
    /// A graceful no-op when the engine is not wired, when mail/CalDAV is off (no
    /// `mail.msek` → nothing to seal a materialized event under, exactly like the
    /// native sink's early return), or when no scheduling channel has been joined.
    /// Per-channel failure is non-fatal (the next tick retries), so this resolves
    /// rather than rejecting — it must never break the receive pass it shares.
    #[wasm_bindgen(js_name = pollScheduling)]
    pub fn poll_scheduling(&self, secret_hex: String, now_secs: f64) -> js_sys::Promise {
        let Some(backend) = self.fauna_mls.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let Some(client) = self.nest_client.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let cursors = self.scheduling_cursors.clone();
        let successions = self.scheduling_successions.clone();
        let anchors_manager = Arc::downgrade(&self.manager);
        let refused = self.manager.refused_changes();
        future_to_promise(async move {
            // Mail/CalDAV off ⇒ no MSEK ⇒ no calendar store to materialize into.
            // The native sink treats this the same way (a no-op the loop retries
            // once the user provisions), so an un-provisioned seat neither errors
            // nor burns its cursor on an iMIP it could not apply.
            let Some(ctx) = crate::rpc::dav_ctx(&secret_hex).await? else {
                return Ok(JsValue::UNDEFINED);
            };
            let resolver = fauna_client_caldav::MemoizedSuccessionResolver {
                addresses: fauna_client_caldav::DiscoveryPrincipalResolver {
                    discovery: fauna_client_caldav::AnonAttendeeDiscovery,
                    own_nest_url: client.nest_url(),
                },
                dialer: WebOrganizerSuccessionDialer {
                    manager: anchors_manager,
                },
                own_nest_url: client.nest_url(),
                memo: successions,
            };
            let sink: Option<Arc<dyn SchedulingSink>> = Some(Arc::new(WebSchedulingSink {
                client,
                ctx,
                now_secs: now_secs as i64,
                refused,
                resolver,
            }));
            // Copy out, poll, write back — never hold the `RefCell` borrow across
            // an await (the `conv_cursors` discipline above).
            let mut owned = cursors.borrow().clone();
            poll_scheduling_feed(&backend, &sink, &mut owned).await;
            *cursors.borrow_mut() = owned;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── Cross-device MLS state sync (devices.md § Cross-device MLS group-state
    //    sync) ──────────────────────────────────────────────────────────────────
    //
    // The web leg of the `fauna.mls.{get,put}` replica plane — the wasm twin of
    // linux `conv_backend.rs`'s `wire_mls_state_sync` + `attach_replica_autosave`.
    // The SPA owns *when* (onMount → `restoreMlsState` before the first poll;
    // debounced after each receive tick / send → `saveMlsState`); the seal +
    // WS-RPC round-trip + the device-owned-epoch gate stay entirely in Rust
    // (priority #2), same split as `restoreDrafts`/`saveDrafts` above.

    /// Restore the cross-device MLS state replica and wire the device-owned-epoch
    /// gate + cursor — the web trigger for the shared
    /// [`fauna_client_mls_sync::orchestration::restore_and_wire_with_retry`]
    /// (design §5 restore-before-first-poll + launch resilience; the same body the
    /// linux leg drives from `conv_backend.rs`). The SPA awaits this once, right
    /// after building the manager (`conversations.ts getConversationsManager`) and
    /// **before** the first `pollConversations`, so the receive loop resumes each
    /// channel from its restored `history/<ch>` watermark (not seq 0) and a gated
    /// send takes over the epoch. A *transient* load failure (nest unreachable at
    /// launch) retries with backoff inside — the promise stays pending and
    /// resolves once the nest is reachable, so a launch blip no longer costs the
    /// tab its cross-device state until reload. Resolves to the number of channels
    /// restored (`0` on a first-run empty rail or the client-less constructor). A
    /// *permanent* failure (e.g. a seal/codec fault or any nest rejection)
    /// rejects after one attempt WITHOUT lifting the [`MlsStateSync`] save gate,
    /// leaving the tab single-device (the un-injected gate is today's optimistic
    /// behavior) and — crucially — never letting a later `saveMlsState` clobber
    /// the user's real nest-stored replica; the SPA logs + swallows.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = restoreMlsState)]
    pub fn restore_mls_state(&self) -> js_sys::Promise {
        let (Some(sync), Some(backend)) = (self.mls_sync.clone(), self.fauna_mls.borrow().clone())
        else {
            return js_sys::Promise::resolve(&JsValue::from_f64(0.0));
        };
        // The retry loop holds `Weak` handles (upgraded per attempt): this object
        // keeps the strong refs for the page's lifetime, and a torn-down wasm
        // session ends an in-flight retry instead of pinning the dropped engine.
        let backend = std::sync::Arc::downgrade(&backend);
        let manager = std::sync::Arc::downgrade(&self.manager);
        future_to_promise(async move {
            use fauna_client_mls_sync::RestoreRetryEnd;
            match fauna_client_mls_sync::orchestration::restore_and_wire_with_retry(
                sync, backend, manager,
            )
            .await
            {
                RestoreRetryEnd::Wired(restored) => Ok(JsValue::from_f64(restored as f64)),
                RestoreRetryEnd::Failed(e) => {
                    Err(JsValue::from_str(&format!("restore mls state: {e}")))
                }
                RestoreRetryEnd::SessionDropped => Err(JsValue::from_str(
                    "restore mls state: session dropped during launch retry",
                )),
            }
        })
    }

    /// Persist the cross-device MLS state replica after an engine/store mutation
    /// (send, inbound-fold, membership) — the web trigger for the shared
    /// [`fauna_client_mls_sync::orchestration`] snapshot/upload split (the same
    /// body linux drives from its debounced `arm_replica_debounce`). The SPA
    /// debounces (calls it after each receive tick + after sends). Snapshots on the
    /// JS thread (engine + manager state lives there) then seals + uploads in the
    /// async body; the `save_history_if_changed`/launch-gate dedup make an
    /// unchanged or pre-restore upload a no-op, so no per-channel dirty tracking is
    /// needed. A no-op before a successful `restoreMlsState` (the gate that prevents
    /// clobbering the user's real replica) or on the client-less constructor.
    /// Resolves to `undefined`; rejects with the transport/seal error (the SPA logs
    /// + swallows — a transient save must not surface on the page). Own-message
    /// history is user-irrecoverable, so a save is never skipped for convenience.
    #[cfg(target_arch = "wasm32")]
    #[wasm_bindgen(js_name = saveMlsState)]
    pub fn save_mls_state(&self) -> js_sys::Promise {
        let (Some(sync), Some(backend)) = (self.mls_sync.clone(), self.fauna_mls.borrow().clone())
        else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let manager = self.manager.clone();
        // Snapshot on the JS thread (engine + manager state lives here); seal +
        // upload in the async body.
        let snapshot =
            fauna_client_mls_sync::orchestration::snapshot_replica(&backend, &manager, &sync);
        future_to_promise(async move {
            fauna_client_mls_sync::orchestration::save_snapshot(&sync, &snapshot)
                .await
                .map_err(|e| JsValue::from_str(&format!("save mls state: {e}")))?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Drain the caller's **durable inbox** once (the web FaunaMls receive
    /// backstop, `api-layers.md` § Inbox & Messaging layer 4): `fauna.inbox.fetch`
    /// → decode each canonical [`InboxEnvelope`] → dispatch by `kind` → `ack` the
    /// durably-applied ids. Welcome items route through the shared
    /// [`ingest_welcome_by_kind`] — a chat Welcome joins + binds its group, a
    /// cross-user folder Welcome is decided by the recipient contact gate
    /// (auto-join / knock / suppress); contact-request / security-notice items have
    /// no web surface yet, so [`WebInboxApply`] leaves them un-acked (no data loss). The
    /// JS receive loop ticks this, then `pollConversations`, each interval — "web
    /// polls while native pushes" (the established mail-rail pattern). Resolves to
    /// the count of items applied this pass (`0` on the receive-only constructor).
    #[wasm_bindgen(js_name = drainInbox)]
    pub fn drain_inbox(&self) -> js_sys::Promise {
        let Some(client) = self.nest_client.borrow().clone() else {
            return js_sys::Promise::resolve(&JsValue::from_f64(0.0));
        };
        let apply = WebInboxApply {
            manager: self.manager.clone(),
            fauna_mls: self.fauna_mls.borrow().clone(),
            nest_client: Some(client.clone()),
        };
        future_to_promise(async move {
            let inbox = fauna_client_inbox::InboxClient::new(client);
            // `page_limit = 0` → the nest's default page size; `drain` pages
            // internally while it makes progress, so one call fully drains.
            match fauna_client_inbox::drain(&inbox, &apply, 0).await {
                Ok(outcome) => Ok(JsValue::from_f64(outcome.applied as f64)),
                Err(e) => Err(JsValue::from_str(&format!("inbox drain: {e}"))),
            }
        })
    }

    // ── Draft persistence (v2) ──────────────────────────────────────────
    //
    // The web leg of draft-persistence v2 (`file-sync.md` § Drafts Sync;
    // conversations.md § Persistence). These two composites keep the snapshot
    // bytes + seal + WS round-trip entirely in Rust — the SPA only decides *when*
    // (onMount → restore; debounced compose change → save), so JS never shuttles
    // the sealed blob (priority #2). Owner-only content; no signing.

    /// Restore the owner's persisted conversations-rail drafts on launch: fetch +
    /// unseal the `__drafts` blob (`fauna.drafts.get`) and hand the canonical
    /// snapshot to the shared `DraftStore`, refreshing observers so the composer
    /// shows them. The SPA awaits this once, right after building the manager
    /// (`conversations.ts getConversationsManager`), so it precedes any
    /// `saveDrafts`. Resolves to `undefined` on a successful restore, a first-run
    /// empty rail (`None`), or the client-less constructor. Any failure — a
    /// present-but-undecryptable blob (seal/AEAD error) OR a transient transport
    /// failure — rejects WITHOUT lifting the [`DraftsSync`] save gate, so a later
    /// `saveDrafts` stays a no-op for the session and can never clobber the user's
    /// real nest-stored drafts; the next launch retries the load.
    ///
    /// The identity epoch is read before the fetch and checked by the restore
    /// (`ConversationsManager::restore_drafts_at`), the uniform shape every app
    /// uses. Web builds a manager per actor, so a late reply already lands in a
    /// discarded instance; the epoch keeps that true if the lifetime changes.
    #[wasm_bindgen(js_name = restoreDrafts)]
    pub fn restore_drafts(&self) -> js_sys::Promise {
        let Some(sync) = self.drafts_sync.clone() else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let mgr = self.manager.clone();
        let epoch = mgr.identity_epoch();
        future_to_promise(async move {
            match sync.load().await {
                Ok(Some(bytes)) => {
                    mgr.restore_drafts_at(epoch, bytes).await;
                    Ok(JsValue::UNDEFINED)
                }
                Ok(None) => Ok(JsValue::UNDEFINED),
                Err(e) => Err(JsValue::from_str(&format!("restore drafts: {e}"))),
            }
        })
    }

    /// Persist the owner's current conversations-rail drafts after a compose
    /// change (the SPA debounces): snapshot the shared `DraftStore` and hand it to
    /// [`DraftsSync::save_if_changed`], which seals under the owner's `BackupKey`
    /// and overwrites the `__drafts` blob (`fauna.drafts.put`) **iff** a launch
    /// restore has succeeded *and* the snapshot differs from the last-saved
    /// baseline. A no-op on the client-less constructor, before/without a
    /// successful restore (the gate that prevents clobbering unread drafts), or
    /// for an unchanged set. Resolves to `undefined`; rejects with the
    /// transport/seal error (the SPA logs + swallows — a transient draft-save
    /// failure must not surface on the page).
    #[wasm_bindgen(js_name = saveDrafts)]
    pub fn save_drafts(&self) -> js_sys::Promise {
        let Some(sync) = self.drafts_sync.clone() else {
            return js_sys::Promise::resolve(&JsValue::UNDEFINED);
        };
        let snapshot = self.manager.drafts_snapshot_bytes();
        future_to_promise(async move {
            sync.save_if_changed(&snapshot)
                .await
                .map(|_| JsValue::UNDEFINED)
                .map_err(|e| JsValue::from_str(&format!("save drafts: {e}")))
        })
    }

    /// Toggle a reaction on `message_id` in `thread_id`
    /// (`dm-reaction-*` / `dm-reaction-add` — conversations.md § Reactions &
    /// message delete). FaunaMls-only; no-op if the thread lacks
    /// `supports_reactions`. Resolves to `undefined`.
    #[wasm_bindgen(js_name = toggleReaction)]
    pub fn toggle_reaction(
        &self,
        thread_id: String,
        message_id: String,
        emoji: String,
    ) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.toggle_reaction(ThreadId(thread_id), MessageId(message_id), emoji)
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Delete `message_id` in `thread_id`
    /// (`dm-message-delete-button` — conversations.md § Reactions & message
    /// delete). FaunaMls-only, sender-only; no-op if the thread lacks
    /// `supports_message_delete` or the message is not owned by the local user.
    /// Resolves to `undefined`.
    #[wasm_bindgen(js_name = deleteMessage)]
    pub fn delete_message(&self, thread_id: String, message_id: String) -> js_sys::Promise {
        let mgr = self.manager.clone();
        future_to_promise(async move {
            mgr.delete_message(ThreadId(thread_id), MessageId(message_id))
                .await;
            Ok(JsValue::UNDEFINED)
        })
    }

    // ── Shared-folder Sharing (owner side) ──────────────────────────────────
    // The web twin of fauna-ffi's `folders_*` free fns (priority #2;
    // folders.md § Sharing). These reuse THIS manager's WS-RPC transport
    // (`nest_client`) + the conversations rail's one per-actor `MlsEngine` (via
    // `FaunaMlsBackend::engine`) as the `FolderGroupCrypto` adapter — never a
    // second engine racing the (in-memory) MLS state. `*_hex` args follow the
    // crate's hex-string convention (`secret_hex` / `parse_actor_hex`).

    /// Share an owner-only folder with one member end-to-end
    /// (`FoldersAuthor::share_set`): fetch the member's KeyPackage → create the MLS
    /// group → `fauna.folders.share` → bind the genesis content key + publish the
    /// envelope → deliver the Welcome. `member_nest_url` is `null` for a same-nest
    /// member. Resolves to `{ channelId: hex, inboxId }`; rejects with the error
    /// string (e.g. the member published no KeyPackage).
    #[wasm_bindgen(js_name = foldersShareSet)]
    pub fn folders_share_set(
        &self,
        owner_secret_hex: String,
        name: String,
        member_id_hex: String,
        member_nest_url: Option<String>,
        access: Option<String>,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let member = parse_actor_hex(&member_id_hex)?;
            let convs = ConversationsClient::new(client.clone());
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            let outcome = author
                .share_set(&convs, &name, member, member_nest_url, access)
                .await
                .map_err(crate::rpc::err_to_js)?;
            crate::rpc::to_js(&ShareOutcomeJs {
                channel_id: hex::encode(outcome.channel_id),
                inbox_id: outcome.inbox_id,
            })
        })
    }

    /// Remove a member from a shared set (`FoldersAuthor::remove_member`): MLS
    /// Remove → rotate the content key → re-publish the envelope under the new epoch
    /// → evict the member from the nest roster → commit. `channel_id_hex` is the
    /// set's derived `ChannelId` (from `foldersShareSet`'s `channelId`). Resolves
    /// to `{ commit: hex|null, evicted, rotated }`.
    #[wasm_bindgen(js_name = foldersRemoveMember)]
    pub fn folders_remove_member(
        &self,
        owner_secret_hex: String,
        name: String,
        channel_id_hex: String,
        member_id_hex: String,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let channel = parse_channel_hex(&channel_id_hex)?;
            let member = parse_actor_hex(&member_id_hex)?;
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            let outcome = author
                .remove_member(&name, channel, member)
                .await
                .map_err(crate::rpc::err_to_js)?;
            crate::rpc::to_js(&RemoveOutcomeJs {
                commit: outcome.commit.map(hex::encode),
                evicted: outcome.evicted,
                rotated: outcome.rotated,
            })
        })
    }

    /// Derive the folder's custody/roster `ChannelId` from its `mls_group_id`
    /// (`ChannelId::from_group_id` — blake3 `derive_key("fauna.channel.v1", …)`).
    /// The owner-side REMOVE flow (`foldersRemoveMember`) takes the derived
    /// `channel_id_hex`, but a set loaded from a fresh snapshot only carries
    /// `mls_group_id`, and this derivation can't be done in TS — so the UI calls
    /// this before `foldersRemoveMember`. The wasm twin of the native
    /// `folder_channel_id_from_group_id` UniFFI face (uniform channel-id shape
    /// across linux/apple/web, priority #1). Resolves synchronously to the 64-hex
    /// `ChannelId`. **NB the real MLS group id is 16 bytes, not 32 — plain
    /// `hex::decode`, never `parse_channel_hex` (which requires exactly 32).**
    #[wasm_bindgen(js_name = foldersChannelIdFromGroupId)]
    pub fn folders_channel_id_from_group_id(
        &self,
        group_id_hex: String,
    ) -> Result<String, JsValue> {
        ChannelId::from_group_id_hex(&group_id_hex)
            .map(|id| hex::encode(id.0))
            .map_err(|e| JsValue::from_str(&format!("invalid MLS group id: {e}")))
    }

    /// Whether this client has actually **joined** the MLS group behind
    /// `group_id_hex` — the browser's local half of the B3 member-list join-filter
    /// (`folders.md` § Sharing, *Member list-visibility*: "a stranger cannot force
    /// a set into your list"). The nest can only report who is *rostered*; only the
    /// engine knows who joined, so the folders bundle's `DevicesMachine.setMlsQuery`
    /// calls back into this. Synchronous, and **fail-safe by construction**:
    /// malformed hex or no wired backend answers `false` (hide the row).
    ///
    /// Lives here rather than in `fauna-wasm-folders` because the per-actor
    /// `MlsEngine` is this bundle's, and a wasm object cannot cross between two
    /// wasm-pack modules.
    #[wasm_bindgen(js_name = foldersIsJoined)]
    pub fn folders_is_joined(&self, group_id_hex: String) -> bool {
        let Some(backend) = self.fauna_mls.borrow().clone() else {
            return false;
        };
        hex::decode(group_id_hex.trim())
            .ok()
            .filter(|b| !b.is_empty())
            .is_some_and(|raw| backend.engine().has_group(&ChannelId::from_group_id(&raw)))
    }

    // ── Recipient side (B5) — the web twins of fauna-ffi's
    // `folders_recipient.rs` free fns (`folders.md` § Sharing — *Recipient
    // side*). The knocked, un-acked `channel_type == "folder"` welcomes the
    // contact gate above left staged ARE the pending-share list; there is no
    // second store. Every one of these reuses the shared `fauna-client-inbox` /
    // `fauna-conversations` logic — web adds only the JS boundary (priority #2).

    /// List the recipient's staged folder shares — the `folder-pending-share`
    /// rows. A **peek**: listing never acks, so a knock survives a page reload.
    /// Resolves to an array of [`PendingShareJs`]; the web twin of
    /// `folders_pending_shares`.
    #[wasm_bindgen(js_name = foldersPendingShares)]
    pub fn folders_pending_shares(&self) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        future_to_promise(async move {
            let Some(client) = client else {
                return crate::rpc::to_js(&Vec::<PendingShareJs>::new());
            };
            let inbox = fauna_client_inbox::InboxClient::new(client);
            let shares =
                fauna_client_inbox::list_folder_pending_shares(&inbox, PENDING_SHARE_PEEK_LIMIT)
                    .await
                    .map_err(|e| JsValue::from_str(&format!("pending shares: {e}")))?;
            let rows: Vec<PendingShareJs> = shares
                .into_iter()
                .map(|s| PendingShareJs {
                    inbox_id: s.inbox_id,
                    shared_by: s.shared_by,
                    shared_by_handle: s.shared_by_handle,
                    shared_by_domain: s.shared_by_domain,
                    shared_by_display: s.shared_by_display,
                    group_id: s.group_id,
                    channel_id: s.channel_id,
                    set_name: s.set_name,
                })
                .collect();
            crate::rpc::to_js(&rows)
        })
    }

    /// Accept a staged share (`folder-share-accept-button`): re-peek to resolve
    /// the Welcome by `inbox_id`, join the MLS group **off the chat rail**
    /// ([`join_folder_welcome`] — no chat thread), then `ack` the durable row.
    /// Accept **bypasses the contact gate on purpose** — the user explicitly
    /// accepted, so no auto/knock/suppress decision applies. Crash-safe: the ack
    /// happens only after a successful (idempotent) join, so a crash between them
    /// re-lists the share and re-accepting is a no-op.
    ///
    /// The peek, the unjoinable refusal and that ordering come from the shared
    /// `fauna_client_folders::accept_folder_share` — the same recipe fauna-ffi's
    /// `folders_accept_share`, tui and linux run, so it is stated once
    /// (priority #2). Only the join is supplied here, because this face holds its
    /// MLS handle as a raw `FaunaMlsBackend` rather than a `ConversationsSession`.
    ///
    /// `inbox_id` is `f64` because a wasm-bindgen `i64` parameter reaches JS as a
    /// `bigint`, which every caller here would have to construct.
    #[wasm_bindgen(js_name = foldersAcceptShare)]
    pub fn folders_accept_share(&self, inbox_id: f64) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        let inbox_id = inbox_id as i64;
        future_to_promise(async move {
            let (client, _engine, backend) = folders_author_parts(client, backend)?;
            let inbox = fauna_client_inbox::InboxClient::new(client);
            fauna_client_folders::accept_folder_share(&inbox, inbox_id, |join| async move {
                let (channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx) =
                    join.into_join_args();
                join_folder_welcome(
                    &backend,
                    &channel_id_hex,
                    &welcome_bytes,
                    &home_nest_url,
                    &welcome_ctx,
                )
                .await
                .map(|_| ())
            })
            .await
            .map_err(|e| JsValue::from_str(&format!("accept share: {e}")))?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Decline a staged share (`folder-share-decline-button`): drop the
    /// recipient's roster row, then `ack` the durable row. The Welcome is dropped
    /// **unprocessed**, so declining never joins the group (`folders.md`
    /// § Sharing); the roster drop keeps the owner's "Shared with" list honest and
    /// a later re-share a genuine re-invite (§ Sharing → *Adding the 2nd..Nth
    /// member*). Both halves, and the load-bearing leave-before-ack ordering, come
    /// from the shared `fauna_client_folders::decline_folder_share` — the same
    /// recipe fauna-ffi's `folders_decline_share` runs (priority #2).
    #[wasm_bindgen(js_name = foldersDeclineShare)]
    pub fn folders_decline_share(&self, inbox_id: f64) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let inbox_id = inbox_id as i64;
        future_to_promise(async move {
            let client = client.ok_or_else(|| {
                JsValue::from_str("decline share: conversations not wired (no nest client)")
            })?;
            fauna_client_folders::decline_folder_share(
                &fauna_client_inbox::InboxClient::new(client.clone()),
                &fauna_client_folders::FoldersClient::new(client),
                inbox_id,
            )
            .await
            .map_err(|e| JsValue::from_str(&format!("decline share: {e}")))?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Leave a set that was shared *with* the caller (`folder-leave-button`) — the
    /// recipient counterpart of `foldersRemoveMember`, and **self-scoped**: it
    /// drops only the caller's own roster row, so it needs no `ownerSecret` and does
    /// **not** rotate the owner's content key (a voluntary leaver keeps the
    /// generations they held, `mls-group-key-material.md` § M2). Addressed by the
    /// raw MLS `group_id` hex from the member-visible `FolderSummary.mls_group_id`.
    ///
    /// Two steps, **nest roster-drop first** (the durable, security-meaningful half —
    /// off the roster their `content_key.get` is denied): `fauna.folders.leave`,
    /// then the local `MlsEngine` forget so the set drops from the `has_group`-filtered
    /// list. Both idempotent, so a crash between them converges on re-run. The web
    /// twin of fauna-ffi's `folders_leave`; the cross-nest home URL resolves from
    /// this session's RAM record only (web wires no foreign-set custody sink, so a
    /// relaunched cross-nest leave falls back to the same-nest path).
    #[wasm_bindgen(js_name = foldersLeave)]
    pub fn folders_leave(&self, group_id_hex: String) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, _engine, backend) = folders_author_parts(client, backend)?;
            let home_url = hex::decode(group_id_hex.trim())
                .ok()
                .filter(|b| !b.is_empty())
                .and_then(|raw| backend.channel_home_url(&ChannelId::from_group_id(&raw)))
                .filter(|u| !u.is_empty());
            FoldersClient::new(client)
                .leave_with_home(group_id_hex.clone(), home_url)
                .await
                .map_err(|e| JsValue::from_str(&format!("leave shared set: {e}")))?;
            leave_folder(&backend, &group_id_hex)
                .map_err(|e| JsValue::from_str(&format!("leave shared set (local): {e}")))?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Re-drive every staged member-removal whose publish was interrupted by a
    /// crash (`FoldersAuthor::resume_pending_removals`). Call on launch so the
    /// rotate-on-removal forward-secrecy guarantee completes. Resolves to the count
    /// resumed.
    ///
    /// `device_id_hex` is the app's recording device — the same id
    /// `foldersServeSet` takes; with it the same launch pass also finishes a
    /// served-set walk a crash or unsynced custody interrupted
    /// (`FoldersAuthor::converge_served_sets`, `webdav-server.md` § Key model (c)).
    ///
    /// This is web's launch folder pass — the twin of the native
    /// `fauna_client_folders::FolderRemovalResume` pass, whose module doc owns
    /// the rule: it needs the MLS restore AND a readable custody, so the SPA
    /// calls it once this tab's account runtime has started (in line when it
    /// already has, else after its start settles — `conversations.ts`). The
    /// pass first re-runs the foreign-home seed the restore ran over a custody
    /// it may not have been able to read; it fills only holes.
    #[wasm_bindgen(js_name = foldersResumePendingRemovals)]
    pub fn folders_resume_pending_removals(
        &self,
        owner_secret_hex: String,
        device_id_hex: Option<String>,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let seeded = gate_backend.seed_channel_homes_from_custody().await;
            if seeded > 0 {
                tracing::info!(
                    "folders: seeded {seeded} channel home(s) from this member's foreign-set \
                     records once custody was readable"
                );
            }
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            // The served-blob follower (`fauna_client_folders::ServedBlobFollower`
            // owns the rule), seeded before the pass's own blob reconcile and
            // started after it — once per tab.
            let follower = fauna_client_folders::ServedBlobFollower::new(
                FoldersClient::new(client.clone()),
                keypair.actor_id(),
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
            )
            .await
            .with_principal_grants(fauna_client_folders::PrincipalFolderGrants::new(
                crate::account_runtime::ledger_seam(),
                &keypair,
            ));
            let mut author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair_from_secret_hex(&owner_secret_hex)?,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            if let Some(device_id) = device_id_hex {
                author =
                    author.with_served_set_converge(fauna_client_folders::served_set_converge(
                        client,
                        &keypair,
                        crate::account_runtime::folder_key_store(actor_hex.clone()),
                        device_id,
                        attested_predecessor_ids(&actor_hex),
                    ));
            }
            let n = author
                .resume_pending_removals()
                .await
                .map_err(crate::rpc::err_to_js)?;
            if !SERVED_BLOB_FOLLOWING.replace(true) {
                let store = crate::account_runtime::folder_key_store(actor_hex);
                match fauna_client_folders::FolderKeyStore::change_notices(&*store).await {
                    Some(notices) => wasm_bindgen_futures::spawn_local(async move {
                        follower.run(notices).await;
                        SERVED_BLOB_FOLLOWING.set(false);
                    }),
                    None => SERVED_BLOB_FOLLOWING.set(false),
                }
            }
            Ok(JsValue::from_f64(n as f64))
        })
    }

    /// Flip a set's WebDAV serve state end-to-end (`FoldersAuthor::serve_set`) —
    /// the web twin of fauna-ffi's `folders_serve_set`, the production caller of
    /// the slice-2 serve orchestration the Settings → Folders
    /// `folder-webdav-toggle` drives: `serve_enable`/`serve_disable`
    /// (content-key genesis/rotation + the nest `webdav_enabled` flag) then
    /// `reconcile_webdav_keys_blob` (the MSEK-sealed `WebdavKeysBlob`
    /// re-provision). `mls_group_id_hex` is the set's raw MLS group id
    /// (`FolderSummary.mls_group_id`) when the set is shared, `null` when
    /// owner-only; the `ChannelId` is derived here. Resolves to the number of
    /// served sets the re-provisioned blob now carries; rejects with the error
    /// string (`no MSEK held …` ⇒ the toggle stays disabled with a set-up-mail
    /// hint — webdav-server.md § Independent enablement).
    ///
    /// `device_id_hex` is the app's recording device (the one its Media
    /// gestures record under); with it, an enable of an unshared set also
    /// re-seals the set's pre-serve files onto the served key before resolving
    /// (the flipping client's walk, `webdav-server.md` § Key model (c)).
    #[wasm_bindgen(js_name = foldersServeSet)]
    pub fn folders_serve_set(
        &self,
        owner_secret_hex: String,
        name: String,
        mls_group_id_hex: Option<String>,
        enable: bool,
        device_id_hex: Option<String>,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let channel_id = match mls_group_id_hex {
                Some(h) => {
                    let raw = hex::decode(&h).map_err(|e| {
                        JsValue::from_str(&format!("mls_group_id must be valid hex: {e}"))
                    })?;
                    Some(ChannelId::from_group_id(&raw).0)
                }
                None => None,
            };
            let mut author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair_from_secret_hex(&owner_secret_hex)?,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            if let Some(device_id) = device_id_hex {
                author =
                    author.with_served_set_converge(fauna_client_folders::served_set_converge(
                        client,
                        &keypair,
                        crate::account_runtime::folder_key_store(actor_hex.clone()),
                        device_id,
                        attested_predecessor_ids(&actor_hex),
                    ));
            }
            let count = author
                .serve_set(&name, channel_id, enable)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::from_f64(count as f64))
        })
    }

    /// Paywall a `web`-mode folder to a subscription tier end-to-end
    /// (`FoldersAuthor::paywall_set`) — the web twin of fauna-ffi's
    /// `folders_paywall_set`, the production caller of slice-6's paywall
    /// orchestration the per-set "paywall to tier" control drives: content-key
    /// genesis → the nest `web_paywall_tier` flag →
    /// the `content.read{folder:set}` grant minted to the nest's web-serve holder
    /// (`monetization.md` § Pillar 2 folder half; `web-content-hosting.md` §
    /// Sealed static files). Each crash point leaves exposure ≤ intent
    /// (sealed-but-flagless ⇒ 404; flagged-but-grantless ⇒ the teaser).
    ///
    /// `mls_group_id_hex` is the set's raw MLS group id
    /// (`FolderSummary.mls_group_id`) when shared, `null` when owner-only (the
    /// serve pseudo-channel is derived by the orchestration). The grant's seal
    /// target — the web-serve holder's X25519 (+ optional ML-KEM ek for the X-Wing
    /// hybrid wrap) — is discovered via the shared
    /// `FoldersAuthor::discover_web_serve_holder`
    /// (`fauna.bridges.fetch_bridge_pubkey` for `("content-processor", "web-serve")`).
    /// Resolves to `undefined`; rejects with the error string (a nest
    /// with no enrolled web-serve holder ⇒ `fauna.bridges.not_found`).
    #[wasm_bindgen(js_name = foldersPaywallSet)]
    pub fn folders_paywall_set(
        &self,
        owner_secret_hex: String,
        name: String,
        tier: String,
        mls_group_id_hex: Option<String>,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let channel_id = match mls_group_id_hex {
                Some(h) => {
                    let raw = hex::decode(&h).map_err(|e| {
                        JsValue::from_str(&format!("mls_group_id must be valid hex: {e}"))
                    })?;
                    Some(ChannelId::from_group_id(&raw).0)
                }
                None => None,
            };
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            let (holder_pubkey, holder_mlkem_ek) = author
                .discover_web_serve_holder()
                .await
                .map_err(crate::rpc::err_to_js)?;
            author
                .paywall_set(&name, &tier, channel_id, holder_pubkey, holder_mlkem_ek)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Re-provision a paywalled set's grant after its content key rotated — the
    /// web twin of fauna-ffi's `folders_rotate_paywall` and the **rotation leg**
    /// of the web-paywall lifecycle (`monetization.md` § Pillar 2 folder half;
    /// `mls-group-key-material.md` § M2: rotation appends the new generation's wrap
    /// via `fauna.capabilities.renew`). Call after a content-key rotation advanced
    /// the set's generation so the web-serve holder gains the new generation's wrap
    /// (an entitled visitor's fresh token then opens the newest-sealed bytes).
    ///
    /// `mls_group_id_hex` + holder discovery mirror
    /// [`foldersPaywallSet`](Self::folders_paywall_set); the grant id is re-derived
    /// inside the orchestration, so no grant-id state crosses the mint→rotate
    /// boundary. Resolves to `undefined`.
    ///
    /// The **automatic** callers live in the orchestration itself (wired
    /// 2026-07-15): rotate-on-removal re-provisions inline and every app
    /// launch runs the keep-alive sweep (`FoldersAuthor::renew_paywalled_grants`,
    /// riding `resume_pending_removals`). This face is the *manual* twin for a
    /// client-driven re-provision.
    #[wasm_bindgen(js_name = foldersRotatePaywall)]
    pub fn folders_rotate_paywall(
        &self,
        owner_secret_hex: String,
        name: String,
        mls_group_id_hex: Option<String>,
    ) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let channel_id = match mls_group_id_hex {
                Some(h) => {
                    let raw = hex::decode(&h).map_err(|e| {
                        JsValue::from_str(&format!("mls_group_id must be valid hex: {e}"))
                    })?;
                    Some(ChannelId::from_group_id(&raw).0)
                }
                None => None,
            };
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            let (holder_pubkey, holder_mlkem_ek) = author
                .discover_web_serve_holder()
                .await
                .map_err(crate::rpc::err_to_js)?;
            author
                .rotate_paywall_grant(&name, channel_id, holder_pubkey, holder_mlkem_ek)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Un-paywall a `web`-mode folder — the web twin of fauna-ffi's
    /// `folders_unpaywall_set` and the **revoke leg** the "clear the paywall"
    /// control drives (`monetization.md` § Pillar 2: revoking a grant darkens
    /// exactly that slice). Revokes the set's grant (the web-serve holder's next
    /// fetch zeroizes the key ⇒ sealed bytes darken to the teaser) and clears the
    /// nest tier flag (a sealed row with no tier fails closed to 404), in that
    /// crash-safe order. Needs **no holder discovery and no `mls_group_id`** —
    /// revoke is keyed on the derived `(owner, grant_id)`, the flag clear by set
    /// name. Resolves to `undefined`.
    #[wasm_bindgen(js_name = foldersUnpaywallSet)]
    pub fn folders_unpaywall_set(&self, owner_secret_hex: String, name: String) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        let backend = self.fauna_mls.borrow().clone();
        future_to_promise(async move {
            let (client, engine, gate_backend) = folders_author_parts(client, backend)?;
            let keypair = keypair_from_secret_hex(&owner_secret_hex)?;
            let actor_hex = keypair.actor_id_hex();
            let author = FoldersAuthor::new(
                FoldersClient::new(client.clone()),
                keypair,
                crate::account_runtime::folder_key_store(actor_hex.clone()),
                crate::account_runtime::mail_store(),
                engine,
            )
            .with_commit_gate(Arc::new(gate_backend))
            .with_grant_log(crate::account_runtime::ledger_seam());
            author
                .unpaywall_set(&name)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// Whether the owner can serve **any** set over WebDAV — the web twin of
    /// fauna-ffi's `folders_can_serve_webdav`, and the capability that gates
    /// [`foldersServeSet`](Self::folders_serve_set). Resolves to a bool; the
    /// folders page renders `folder-webdav-toggle` disabled with a "set up
    /// mail first" hint when it is `false`, so the actor never clicks into the
    /// `no MSEK held …` rejection (webdav-server.md § Independent enablement
    /// point 2).
    ///
    /// Needs only the nest connection + the owner secret — **not** the MLS
    /// engine the serve itself rides — so the page can ask at render time.
    #[wasm_bindgen(js_name = foldersCanServeWebdav)]
    pub fn folders_can_serve_webdav(&self, owner_secret_hex: String) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        future_to_promise(async move {
            let client = client.ok_or_else(|| {
                JsValue::from_str(
                    "folders: conversations not wired (build via WsRpcClient.conversationsManager)",
                )
            })?;
            let _ = (client, keypair_from_secret_hex(&owner_secret_hex)?);
            let can = fauna_client_folders::owner_can_serve_webdav(
                crate::account_runtime::mail_store().as_ref(),
            )
            .await
            .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::from_bool(can))
        })
    }

    // ── Unattested-member review ──
    //
    // The permanent Settings sub-page's data seam — the wasm twin of
    // fauna-ffi's `member_review.rs` (zero logic owed, priority #2).
    // `memberReviewRemove` never takes a verdict: it always evicts first and
    // persists only what the eviction earned, so this app cannot record
    // `Removed` from its own reasoning — the one seam that can write it calls
    // the one function that can earn it.

    /// Read the open review roster (`fauna_client_config::load_member_reviews`).
    #[wasm_bindgen(js_name = memberReviewList)]
    pub fn member_review_list(&self) -> js_sys::Promise {
        future_to_promise(async move {
            let store = crate::account_runtime::ledger_store()?;
            let reviews = load_member_reviews(&store)
                .await
                .map_err(crate::rpc::err_to_js)?;
            crate::rpc::to_js(
                &reviews
                    .into_iter()
                    .map(MemberReviewJs::from)
                    .collect::<Vec<_>>(),
            )
        })
    }

    /// The shared row-text parts for one review item — consumed, never
    /// re-derived (`fauna_core::data::review_row_text`'s reason-selection and
    /// unnameable-person rules live once, shared). `handle` must be resolved
    /// BEFORE a `memberReviewRemove` call for the same person:
    /// `handleForPerson` reads live membership, so there is no seat left to
    /// read one off afterward.
    #[wasm_bindgen(js_name = memberReviewRowText)]
    pub fn member_review_row_text(
        &self,
        person_hex: String,
        reasons: Vec<String>,
        handle: Option<String>,
    ) -> Result<JsValue, JsValue> {
        let review = MemberReview {
            person: parse_actor_hex(&person_hex)?,
            reasons: reasons
                .iter()
                .map(|r| MemberUnattestedReason::from(r.as_str()))
                .collect(),
        };
        crate::rpc::to_js(&review_row_text(&review, handle.as_deref()))
    }

    /// The review mark each of `threadDetail(threadId).participant_displays`
    /// carries — **index-parallel with that list**, hence with the
    /// `thread-member-chip[i]` those displays render: the flagged person's
    /// actor id (hex) at a chip under open review, `null` at every other chip.
    /// `null` in place of the whole array for an unknown thread id, exactly as
    /// `threadDetail` answers.
    ///
    /// **It answers with the id rather than a bare boolean on purpose.** The
    /// surface that renders a mark is also the surface that presses *Keep*, and
    /// `memberReviewKeep` is keyed on the person — so a boolean would send the
    /// caller straight back to the participant list to hex-encode an
    /// `ActorId(pub [u8; 32])` that `Serialize` delivers to JS as a 32-number
    /// array. Answering both halves at once is what keeps identity mapping out
    /// of every view layer that crosses this boundary. A roster edit keyed on
    /// the rendered handle would be worse still: an MLS roster's handles are
    /// empty by construction and attacker-chosen by threat model
    /// (`succession-aftermath.md` § Propagation → *Removing a flagged member*,
    /// rule 1).
    ///
    /// `rosterPersonHexes` is the caller's **cached** `memberReviewList` result
    /// projected to its `person` field — the roster is cached, not re-read per
    /// paint (§ Propagation → *MLS groups*), which is why this is a synchronous
    /// getter and not a promise. The decision itself stays in the one place:
    /// `fauna_conversations::member_review_flags` over
    /// `fauna_core::data::is_under_review`, the same projection the native
    /// member lists ask per row.
    #[wasm_bindgen(js_name = memberReviewMarksForThread)]
    pub fn member_review_marks_for_thread(
        &self,
        thread_id: String,
        roster_person_hexes: Vec<String>,
    ) -> Result<JsValue, JsValue> {
        let Some(detail) = self.manager.thread_detail(ThreadId(thread_id)) else {
            return Ok(JsValue::NULL);
        };
        // `reasons` plays no part in `is_under_review` (presence is the whole
        // question), so the roster is rebuilt from the ids alone rather than
        // asking every caller to round-trip a field the join never reads.
        let roster = roster_person_hexes
            .iter()
            .map(|hex| {
                Ok(MemberReview {
                    person: parse_actor_hex(hex)?,
                    reasons: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?;
        let flags = fauna_conversations::member_review_flags(&detail.participants, &roster);
        let marks: Vec<Option<String>> = detail
            .participants
            .iter()
            .zip(flags)
            .map(|(addr, flagged)| {
                // `flagged` is only ever true for an address that HAS an actor
                // id, so the inner `None` is unreachable rather than a silent
                // drop — and it stays expressed as one so a future rail change
                // cannot turn it into a wrong-person mark.
                flagged
                    .then(|| addr.person_actor_id().map(|p| p.to_hex()))
                    .flatten()
            })
            .collect();
        crate::rpc::to_js(&marks)
    }

    /// Each participant's actor id (hex), **index-parallel with
    /// [`ThreadDetail::participant_displays`]** and therefore with the
    /// `thread-member-chip[i]` those displays render — `null` for a
    /// participant on a rail that carries no actor id.
    ///
    /// The member chip is the *Remove* control on a membership-change-capable
    /// thread (`conversations.md` § the `thread-member-chip[i]` row), and
    /// `removeParticipant` keys both of its halves on the actor id. So this is
    /// what lets a view layer zip the identity onto the chip **at paint** and
    /// capture it in the chip's own handler, the way tui carries the row's own
    /// address rather than an index: a roster that shifts between paint and
    /// tap then removes the person the chip named, instead of whoever landed
    /// on that position. Removing by index is the wrong-person bug this
    /// deliberately makes unrepresentable.
    ///
    /// Hex, not the raw id, for [`memberReviewMarksForThread`]'s reason: an
    /// `ActorId(pub [u8; 32])` reaches JS as a 32-number array, and re-encoding
    /// it in every view layer is the identity mapping this boundary exists to
    /// keep out of them. The handle half `removeParticipant` also takes is
    /// already on the chip — `participant_displays[i]` IS the handle for a
    /// Fauna address ([`TypedAddress::display`]) — and is cosmetic to the
    /// removal itself, mattering only so a rolled-back removal restores the
    /// row it took away.
    #[wasm_bindgen(js_name = participantActorIdsForThread)]
    pub fn participant_actor_ids_for_thread(&self, thread_id: String) -> Result<JsValue, JsValue> {
        let Some(detail) = self.manager.thread_detail(ThreadId(thread_id)) else {
            return Ok(JsValue::NULL);
        };
        let ids: Vec<Option<String>> = detail
            .participants
            .iter()
            .map(|addr| addr.person_actor_id().map(|p| p.to_hex()))
            .collect();
        crate::rpc::to_js(&ids)
    }

    /// The handle `person` is seated under, in the owner's own conversations —
    /// `null` when conversations are not up yet or they hold no seat this
    /// manager can name.
    #[wasm_bindgen(js_name = handleForPerson)]
    pub fn handle_for_person(&self, person_hex: String) -> Result<Option<String>, JsValue> {
        let person = parse_actor_hex(&person_hex)?;
        Ok(self.manager.handle_for_person(&person))
    }

    /// Record **Keep** — closes every open item for `person` with no group
    /// changes. Resolves to whether anything was actually open (a concurrent
    /// device may have already answered — a success no-op, never a rejection).
    #[wasm_bindgen(js_name = memberReviewKeep)]
    pub fn member_review_keep(&self, person_hex: String) -> js_sys::Promise {
        future_to_promise(async move {
            let person = parse_actor_hex(&person_hex)?;
            let store = crate::account_runtime::ledger_store()?;
            let changed = decide_member_review(&store, &person, UnattestedVerdict::Kept)
                .await
                .map_err(crate::rpc::err_to_js)?;
            Ok(JsValue::from_bool(changed))
        })
    }

    /// Record **Remove** — evicts `person` from every group of the owner's
    /// they are in NOW (re-derived, never from the stored item), then
    /// persists only whatever verdict the eviction earned. A partial
    /// eviction earns none: the review item stays open, and the resolved
    /// `CrossGroupEviction`'s `evicted`/`failed`/`unreachable` fields are
    /// what the caller composes its own message from — the derivation is
    /// shared, the wording per-app.
    #[wasm_bindgen(js_name = memberReviewRemove)]
    pub fn member_review_remove(&self, person_hex: String) -> js_sys::Promise {
        let manager = self.manager.clone();
        future_to_promise(async move {
            let person = parse_actor_hex(&person_hex)?;
            let eviction = manager.evict_person_everywhere(&person).await;
            if let Some(verdict) = eviction.earned_verdict() {
                let store = crate::account_runtime::ledger_store()?;
                decide_member_review(&store, &person, verdict)
                    .await
                    .map_err(crate::rpc::err_to_js)?;
            }
            crate::rpc::to_js(&eviction)
        })
    }

    // ── Nostr succession-aftermath npub confirm ──
    //
    // The Nostr page's data seam — the wasm twin of fauna-ffi's
    // `nostr_npub_confirm.rs` (zero logic owed, priority #2).

    /// Is the caller owed an npub confirmation right now — the Nostr page's
    /// nav-enter read (tui `nostr.rs::refresh_and_check_npub` is the
    /// reference). Best-effort like the shared function itself: any unhappy
    /// answer — no account runtime in this tab yet included — degrades to
    /// `false` rather than rejecting. The stamp is a local read of the
    /// account plane (`fauna.state.nostr-confirmation`); `owner_secret_hex`
    /// is no longer read and stays only so the JS signature is unchanged.
    #[wasm_bindgen(js_name = npubConfirmationOwed)]
    pub fn npub_confirmation_owed(&self, owner_secret_hex: String) -> js_sys::Promise {
        let client = self.nest_client.borrow().clone();
        future_to_promise(async move {
            let client = client.ok_or_else(|| {
                JsValue::from_str(
                    "nostr npub-confirm: conversations not wired (build via WsRpcClient.conversationsManager)",
                )
            })?;
            let account = crate::account_runtime::handle();
            let owed = npub_confirmation_owed_for(
                &client,
                account.as_ref().map(|a| a.npub_confirmed_at()),
            )
            .await;
            Ok(JsValue::from_bool(owed))
        })
    }

    /// Record the owner's "yes, that's my npub" confirmation — also the
    /// best-effort call a fresh (re-)link makes on its own, so the banner
    /// never resurrects itself right after a deliberate re-link (tui's
    /// `Op::Link`, linux's link-success arm are the reference).
    ///
    /// `now_secs` is the caller's own clock (epoch seconds) — passed in
    /// rather than read here, same as the shared function. The write is an
    /// account-plane put (`fauna.state.nostr-confirmation`) through the tab's
    /// runtime handle, rejected while none is running; `owner_secret_hex` is
    /// no longer read and stays only so the JS signature is unchanged.
    #[wasm_bindgen(js_name = confirmNostrNpub)]
    pub fn confirm_nostr_npub(&self, owner_secret_hex: String, now_secs: f64) -> js_sys::Promise {
        future_to_promise(async move {
            let account = crate::account_runtime::handle().ok_or_else(|| {
                JsValue::from_str("nostr npub-confirm: the account store is not ready yet")
            })?;
            account
                .confirm_nostr_npub(now_secs as i64)
                .await
                .map_err(|e| JsValue::from_str(&format!("nostr npub-confirm: {e:#}")))?;
            Ok(JsValue::UNDEFINED)
        })
    }
}

/// The web [`fauna_client_inbox::InboxApply`] impl `drainInbox` dispatches to.
///
/// Every Welcome — DM, group, scheduling, **and cross-user folder** — routes
/// through the one shared [`ingest_welcome_by_kind`] dispatch the native receive
/// rail uses (`api-layers.md` § Inbox & Messaging — "reuse that code, don't fork
/// it"), so the recipient **contact gate** is literally the same code path on web
/// as on the five native apps: a `folder` Welcome is decided by
/// [`NestFolderGate`] over `fauna.contacts.status`
/// (`docs/goal/ui/folders.md` § Sharing — *Recipient side*). Auto (a Confirmed /
/// Accepted contact) joins off the chat rail and acks; Knock (stranger / Pending /
/// unstamped / any lookup failure) returns `Err`, which the drain absorbs as a
/// retained skip — the un-acked Welcome **is** the `folder-pending-share`;
/// Suppress (a Blocked sharer) acks-and-drops.
///
/// Contact-request / security-notice items have no web surface yet, so they return
/// `Err` — the drain absorbs that as a skip and leaves the item **un-acked**
/// (never dropped; the alpha no-user-data-loss rule), so a later web build with
/// those surfaces drains them. A DM/group Welcome on the same page still applies +
/// acks regardless.
#[cfg(target_arch = "wasm32")]
struct WebInboxApply {
    manager: Arc<ConversationsManager>,
    fauna_mls: Option<Arc<FaunaMlsBackend>>,
    /// Backs the folder contact gate's `fauna.contacts.status` lookup. `None`
    /// on the client-less constructor ⇒ no gate ⇒ the shared dispatch retains
    /// every folder Welcome un-acked (its own documented fail-safe).
    nest_client: Option<fauna_rpc_wasm::WsRpcClient>,
}

#[cfg(target_arch = "wasm32")]
impl fauna_client_inbox::InboxApply for WebInboxApply {
    type Error = String;

    async fn apply_welcome(
        &self,
        welcome: fauna_protocol::inbox::WelcomeInbox,
    ) -> Result<(), String> {
        let backend = self
            .fauna_mls
            .as_ref()
            .ok_or_else(|| "no FaunaMls backend wired".to_string())?;
        let kind =
            wire_channel_type_to_kind(welcome.channel_type.clone(), welcome.group_id.clone());
        // The gate is built per drained Welcome (it is a stateless wrapper over the
        // same WS-RPC transport) and handed to the shared dispatch, which reads it
        // only on the `Folder` arm. Client-less ⇒ `None` ⇒ that arm's fail-safe
        // "gate not registered, retain un-acked" branch.
        let gate: Option<Arc<dyn FolderGateSink>> = self.nest_client.clone().map(|c| {
            let gate: Arc<dyn FolderGateSink> = Arc::new(NestFolderGate::new(c));
            gate
        });
        let channel_id = welcome.channel_id.unwrap_or_default();
        let home_nest_url = welcome.nest_url.unwrap_or_default();
        let welcome_ctx = FolderWelcomeContext {
            shared_by: welcome.shared_by,
            set_name: welcome.set_name,
            // The home-nest-resolved access grant — recorded into the
            // accept-time foreign-set record so the web app knows whether
            // this set is writer-granted. Advisory-for-UI, never an authz input.
            access: welcome.access,
            // The home nest's deployment identity (byte-plane pin trust root) +
            // the owner-chosen cadence, recorded on the foreign-set record.
            home_nest_actor_id: welcome.home_nest_actor_id,
            // The cross-nest owner label, recorded on the foreign-set record
            // when its own nest verified the pair.
            shared_by_handle: welcome.shared_by_handle,
            shared_by_domain: welcome.shared_by_domain,
            // The sealed set name — what names a cross-nest set once the join
            // holds its content keys.
            set_name_seal: fauna_core::label_custody::SealedSetName::from_wire(
                welcome.set_name_sealed.as_deref().map(|b| &b[..]),
                welcome.set_name_hash.as_deref().map(|b| &b[..]),
            ),
        };
        ingest_welcome_by_kind(
            backend,
            &self.manager,
            &kind,
            &channel_id,
            &welcome.welcome_bytes,
            &home_nest_url,
            &welcome_ctx,
            &gate,
        )
        .await
        // Same door the push arm reports through — web has no push arm, so the drain is the
        // ONLY Welcome ingest path here; without this call every outcome on
        // this platform was silent, benign or hostile alike.
        .inspect_err(|e| fauna_conversations::session::report_welcome_ingest_failure(&kind, e))
        .map_err(|e| format!("ingest welcome: {e}"))
    }

    async fn apply_contact_request(&self, _tuple_bytes: Vec<u8>) -> Result<(), String> {
        // No web contacts-request surface yet — leave un-acked (no data loss).
        Err("web contact-request surface not yet built — left in inbox".to_string())
    }

    async fn apply_security_notice(
        &self,
        _notice: fauna_protocol::inbox::SecurityNoticeInbox,
    ) -> Result<(), String> {
        // Honest ack: the nest writes the render surface itself — the
        // `notifications` row the Notifications page lists on every app
        // (`notifications.md` § Security notices) — so this inbox envelope is a
        // redundant copy and consuming it loses nothing.
        Ok(())
    }
}

// ── Opening one sealed mail record ─────────────────────────────────────────

/// Open ONE sealed mail record — the two-layer `inbox.fetch` / `sent.fetch`
/// shape — to its RFC 5322 plaintext under the manager's held recipient keys.
/// Shared by [`WasmConversationsManager::ingest_sealed_inbound`] and the
/// attachment refill's re-read ([`WasmConversationsManager::refill_mail_record`]),
/// so a re-read cannot open differently from the first delivery. The two miss
/// arms are distinct on purpose ([`OpenMiss`]): no key set is a caller-order
/// fault, an unopenable record is a fact about the record the ingest skips
/// past (`mail-app-surface.md` § Inbound client receive → *Unopenable records*).
fn open_sealed_mail_record(
    manager: &WasmConversationsManager,
    uid: u32,
    stored_at_secs: f64,
    sealed_envelope: &[u8],
) -> Result<Vec<u8>, OpenMiss> {
    let keypairs = manager.standing_keypairs.borrow();
    if keypairs.is_empty() {
        return Err(OpenMiss::NoKeys);
    }
    // Content-sealing-epochs: classify the seal epoch from the record's SEAL
    // INSTANT (`stored_at`, the `InboxMessage.stored_at` wire field) — never
    // `internal_date` (imported mail diverges by design) nor
    // the sender-supplied `Date:` header. The unknown `0` is a standing-sealed
    // record, which the chain's standing arm opens. `f64` param (not `i64`) to
    // keep the JS boundary a `number`, not a `BigInt`.
    let seal_instant = (stored_at_secs as i64).max(0) as u64;
    let epoch_roots_guard = manager.mail_epoch_roots.borrow();
    let epoch_roots: Vec<&[u8; 32]> = epoch_roots_guard.iter().collect();
    // The one shared opener the native page opener uses too: the epoch chain
    // over the roots, then the standing arm over the complete key set (current
    // + grace generations, either suite per keypair), so standing-sealed and
    // pre-rotation mail open exactly as they do on every other app.
    // No retirement instants cross the JS seam yet (`setRecipientKeypairs`
    // carries the keypairs alone): the whole set walks newest first, which
    // opens every generation — only the seal-time ordering is native-only.
    fauna_mail::open_inbound_record_with_keys(
        sealed_envelope,
        &epoch_roots,
        seal_instant,
        &keypairs,
        &[],
    )
    .map_err(|e| OpenMiss::Unopenable(format!("open inbound record (uid {uid}): {e}")))
}

/// Why [`open_sealed_mail_record`] returned no plaintext.
enum OpenMiss {
    /// No standing key set held yet — the JS loop gates on `hasRecipientSecret`.
    NoKeys,
    /// The record does not open under the complete key set — deterministic,
    /// so a retry with the same keys cannot differ; the ingest skips it.
    Unopenable(String),
}

impl From<OpenMiss> for JsValue {
    fn from(miss: OpenMiss) -> Self {
        match miss {
            OpenMiss::NoKeys => JsValue::from_str("mail not enabled: recipient MSEK not set"),
            OpenMiss::Unopenable(reason) => JsValue::from_str(&reason),
        }
    }
}

/// The web receive loop's mailbox spelling — [`MailFeed`]'s serialized form,
/// `"inbox"` or `"sent"` — as the shared type.
fn mail_feed_from_js(mailbox: &str) -> Result<MailFeed, JsValue> {
    match mailbox {
        "inbox" => Ok(MailFeed::Inbox),
        "sent" => Ok(MailFeed::Sent),
        other => Err(JsValue::from_str(&format!(
            "unknown mailbox {other:?} (expected \"inbox\" or \"sent\")"
        ))),
    }
}

// ── The client-feed reference leg: fetching a referenced body back ───────────
//
// `smtp-server.md` § Message size limits. The rule itself (hash width → store
// key → fetch → fail-closed join) is `fauna_mail::body_ref`, shared with the
// native receive path; only the browser's fetch leg lives here.

#[cfg(target_arch = "wasm32")]
async fn resolve_mail_body_ref_inner(
    nest_url: &str,
    chunk_hashes_concat: &[u8],
    total_bytes: f64,
) -> anyhow::Result<Vec<u8>> {
    if total_bytes < 0.0 {
        anyhow::bail!("total_bytes must not be negative");
    }
    // The web twin of native `fauna_client::NestPublicChunkFetcher` — the same
    // open chunk-download route, shared with the Backups per-file download
    // (`snapshot_download`) and `fauna-media-machine`'s wasm build. Its
    // `fetch_manifest` is never called here: a mail body reference is an
    // ordered hash list with no manifest object
    // (`fauna_mail::body_ref` module docs) — `resolve_referenced_mail_body`
    // only calls `fetch_chunks`.
    let fetcher =
        fauna_core::file_download::WasmPublicChunkFetcher::new(nest_url.trim_end_matches('/'));
    // `chunks(32)` re-splits the concatenated reference; a tail that isn't 32
    // bytes reaches the shared resolver as a short hash and is refused there,
    // so the width rule stays in one place.
    let hashes: Vec<&[u8]> = chunk_hashes_concat.chunks(32).collect();
    fauna_mail::body_ref::resolve_referenced_mail_body(&fetcher, &hashes, total_bytes as u64).await
}

/// The succession ceremony's reach into the live conversations session.
///
/// Kept out of the `#[wasm_bindgen]` impl deliberately: this is a Rust-side seam
/// between two wasm modules, not a JS face. The engine and the sync it must be
/// persisted through never cross into JS — the SPA passes the *manager* to
/// `succeedIdentityWithHeldKit` and this assembles the pair on this side.
#[cfg(target_arch = "wasm32")]
impl WasmConversationsManager {
    /// This session's MLS engine plus everything needed to land its state, or
    /// `None` when conversations were never wired (the receive-only / SMTP-only
    /// constructors, or a tab whose `restoreMlsState` never ran).
    ///
    /// `None` is a legitimate ceremony outcome, not a failure: it is exactly
    /// native's `SweepStatus::NoEngine` arm — conversations were not up, so there
    /// was no engine to sweep from.
    pub(crate) fn succession_engine(&self) -> Option<crate::succession::OldEngineHandle> {
        let backend = self.fauna_mls.borrow().clone()?;
        let sync = self.mls_sync.clone()?;
        Some(crate::succession::OldEngineHandle {
            engine: backend.engine(),
            backend,
            manager: self.manager.clone(),
            sync,
        })
    }
}

// ── The room model's render mappings (`ui/conversations.md` § Element IDs) ──
//
// The wasm twins of the FFI twins in `fauna_conversations::room` — the SAME
// mappings linux and tui call directly and apple/android/windows reach through
// UniFFI, so no painter hand-types a token, a label or an editor's choice list
// (priority #2). They take the **serde spelling the state JSON already
// carries** (`"EndToEnd"`, `"Owner"`, `"Invite"`, `"Full"` — `state_json`'s
// `format!("{:?}")` and serde's unit-variant name are the same string), so the
// SPA feeds a helper what it just read off the thread and never maintains a
// parallel table.

/// The class's localized sentence — what `thread-room-class` and
/// `recipient-picker-class` state (`RoomClass::label`).
#[wasm_bindgen(js_name = roomClassLabel)]
pub fn room_class_label(class: JsValue) -> Result<String, JsValue> {
    let class: fauna_conversations::room::RoomClass = crate::rpc::from_js(class)?;
    Ok(fauna_conversations::room::room_class_label(class))
}

/// The class's driver-facing `class` attribute — `end-to-end` / `community` /
/// `transport-only` (`RoomClass::attr_token`).
#[wasm_bindgen(js_name = roomClassAttrToken)]
pub fn room_class_attr_token(class: JsValue) -> Result<String, JsValue> {
    let class: fauna_conversations::room::RoomClass = crate::rpc::from_js(class)?;
    Ok(fauna_conversations::room::room_class_attr_token(class))
}

/// The family gate's marker text — what `conversation-guardian-state` states
/// on a bridged room's row and detail (`GuardianState::label`).
#[wasm_bindgen(js_name = guardianStateLabel)]
pub fn guardian_state_label(state: JsValue) -> Result<String, JsValue> {
    let state: fauna_conversations::snapshot::GuardianState = crate::rpc::from_js(state)?;
    Ok(state.label().to_string())
}

/// The marker's driver-facing `state` attribute — `held` / `blocked`
/// (`GuardianState::attr_token`).
#[wasm_bindgen(js_name = guardianStateAttrToken)]
pub fn guardian_state_attr_token(state: JsValue) -> Result<String, JsValue> {
    let state: fauna_conversations::snapshot::GuardianState = crate::rpc::from_js(state)?;
    Ok(state.attr_token().to_string())
}

/// A member chip's `role` attribute — `owner` / `admin` / `member`
/// (`RoomRole::attr_token`).
#[wasm_bindgen(js_name = roomRoleAttrToken)]
pub fn room_role_attr_token(role: JsValue) -> Result<String, JsValue> {
    let role: fauna_conversations::room::RoomRole = crate::rpc::from_js(role)?;
    Ok(fauna_conversations::room::room_role_attr_token(role))
}

/// A member chip's text: the display name, plus the localized owner/admin mark
/// on a governed room (`member_chip_text`). `role` is `null` for a plain
/// member and for every seat of a policy-less room.
#[wasm_bindgen(js_name = roomMemberChipText)]
pub fn room_member_chip_text(display: String, role: JsValue) -> Result<String, JsValue> {
    let role: Option<fauna_conversations::room::RoomRole> = if role.is_null() || role.is_undefined()
    {
        None
    } else {
        Some(crate::rpc::from_js(role)?)
    };
    Ok(fauna_conversations::room::room_member_chip_text(
        display, role,
    ))
}

/// The rules `room-join-rule-select` offers, in its order, as
/// `[{token, label}]` — one list for all seven editors.
#[wasm_bindgen(js_name = roomJoinRuleEditorChoices)]
pub fn room_join_rule_editor_choices() -> Result<JsValue, JsValue> {
    let choices: Vec<_> = fauna_conversations::room::room_join_rule_editor_choices()
        .into_iter()
        .map(|rule| {
            serde_json::json!({
                "token": rule.token(),
                "label": fauna_conversations::room::room_join_rule_label(rule),
            })
        })
        .collect();
    crate::rpc::to_js(&choices)
}

/// The policies `room-history-policy-select` offers, in its order, as
/// `[{token, label}]`.
#[wasm_bindgen(js_name = roomHistoryPolicyEditorChoices)]
pub fn room_history_policy_editor_choices() -> Result<JsValue, JsValue> {
    let choices: Vec<_> = fauna_conversations::room::room_history_policy_editor_choices()
        .into_iter()
        .map(|policy| {
            serde_json::json!({
                "token": policy.token(),
                "label": fauna_conversations::room::room_history_policy_label(policy),
            })
        })
        .collect();
    crate::rpc::to_js(&choices)
}

/// The class of the room the new-thread picker is about to create, once a
/// recipient chip is committed (`recipient-picker-class`) — `null` before the
/// first chip. `chips` is the committed chip list; `include_home_nest` says
/// whether this room's home nest is itself a member
/// (`prospective_room_class`).
#[wasm_bindgen(js_name = roomProspectiveClass)]
pub fn room_prospective_class(chips: JsValue, include_home_nest: bool) -> Result<JsValue, JsValue> {
    let chips: Vec<TypedAddress> = crate::rpc::from_js(chips)?;
    match fauna_conversations::room::room_prospective_class(chips, include_home_nest) {
        Some(class) => crate::rpc::to_js(&class),
        None => Ok(JsValue::NULL),
    }
}

/// `room-join-rule-select`'s token for a rule (`JoinRule::token`) — the value
/// the picker round-trips. A UniFFI app reads the draft's `join_rule` field as
/// a typed enum and maps it with its own twin; web reads the same field off
/// the serialized draft and maps it here, so neither hand-types the token.
#[wasm_bindgen(js_name = roomJoinRuleToken)]
pub fn room_join_rule_token(rule: JsValue) -> Result<String, JsValue> {
    let rule: fauna_conversations::room::JoinRule = crate::rpc::from_js(rule)?;
    Ok(rule.token().to_string())
}

/// `room-history-policy-select`'s token for a policy (`HistoryPolicy::token`).
#[wasm_bindgen(js_name = roomHistoryPolicyToken)]
pub fn room_history_policy_token(policy: JsValue) -> Result<String, JsValue> {
    let policy: fauna_conversations::room::HistoryPolicy = crate::rpc::from_js(policy)?;
    Ok(policy.token().to_string())
}
