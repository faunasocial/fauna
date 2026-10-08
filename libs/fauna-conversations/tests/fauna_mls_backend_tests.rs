//! `FaunaMlsBackend` end-to-end tests:
//! Track A (send on a bound channel), Track B (lazy group bootstrap on an
//! unbound new thread + the inbound decrypt driver `poll_inbound_conv`), and
//! Track C (receiver-side `ingest_welcome` → materialize + bind the thread).
//!
//! These are shared-Rust tests with zero nest and zero client-side MLS: real
//! in-process `MlsEngine`s do the crypto, and a `MockNest` stands in for the
//! `fauna.conversations.*` WS-RPC kinds (channel send/fetch, keypackage
//! fetch/upload, welcome deliver). The MockNest is a faithful in-memory model of
//! the nest's channel log + keypackage queues, so two engines can form a group
//! and exchange messages entirely through the seam.

use async_trait::async_trait;
use fauna_client_moderation::LocalDetectionStore;
use fauna_conversations::Rail;
use fauna_conversations::address::TypedAddress;
use fauna_conversations::attachment_blocks;
use fauna_conversations::backend::{
    BackendError, ConvPushEvent, ConvRpcError, ConversationsPush, ConversationsRpc,
    FolderCustodySink, FolderGateSink, InboundMailPage, InboundMailRecord, InboundMailSource,
    InboxDrainSource, OutboundMailSink, RailBackend, RailInboundMessage, ResolveResult,
    ResolvedAttachment, SchedulingSink, UnboundChannelClass, UnboundSeat, WelcomeChannelKind,
    WelcomeNudge,
};
use fauna_conversations::backends::fauna_mls::{
    FaunaMlsBackend, ingest_scheduling_welcome, ingest_welcome, join_folder_welcome, leave_folder,
    poll_inbound_conv, poll_inbound_folder, poll_inbound_scheduling, redrive_parked_successions,
    refill_evicted_attachments, settle_parked_successions,
};
use fauna_conversations::capabilities::derive_capabilities;
use fauna_conversations::compose::{ComposeState, ResolveState, SendState};
use fauna_conversations::manager::ConversationsManager;
use fauna_conversations::message::{BodyFormat, MessageBadges, MessageId};
use fauna_conversations::observer::SnapshotObserver;
use fauna_conversations::session::{ConversationsSession, FolderWelcomeContext};
use fauna_conversations::snapshot::ThreadDetail;
use fauna_conversations::store::history::ChannelHistorySlice;
use fauna_conversations::thread::{ThreadFlavor, ThreadId};

use fauna_core::data::{ArrivalDisposition, Timestamp};
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_mls::engine::MlsEngine;
use fauna_mls::types::{
    ChannelEnvelope, ChannelId, ChannelMessage, ChannelMessageBody, GroupMetaMessage, ReactionOp,
};

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// One captured `welcome.deliver` call.
#[derive(Clone)]
struct WelcomeCall {
    recipient_hex: String,
    channel_hex: String,
    welcome_bytes: Vec<u8>,
    kind: WelcomeChannelKind,
    /// `Some(domain)` when the seam routed this Welcome cross-nest (the peer's
    /// foreign nest), `None` for same-nest.
    peer_domain: Option<String>,
}

/// An in-memory model of the nest's `fauna.conversations.*` surface — enough for
/// two engines to bootstrap a group and exchange ciphertext through the seam.
#[derive(Default)]
struct MockNest {
    /// `channel_hex → ordered envelopes`; the server sequence is `index + 1`.
    channels: Mutex<HashMap<String, Vec<Vec<u8>>>>,
    /// `channel_hex → per-send plaintext `attachment_refs``, parallel to
    /// `channels` — what the real nest records beside each mirror row (the
    /// conversation kind's blob-reachability floor, `encryption-at-rest.md`
    /// § Per-content-kind conformance → Conversation messages row).
    attachment_refs: Mutex<HashMap<String, Vec<Vec<String>>>>,
    /// `actor_hex → queued TLS key-package bytes` (FIFO, consumed by fetch).
    keypackages: Mutex<HashMap<String, VecDeque<Vec<u8>>>>,
    /// `actor_hex → the single reusable last-resort key package` (Spec Y2). A
    /// last-resort upload REPLACES this (one per actor, mirroring the nest's
    /// `put_last_resort_key_package`), so re-publishing on every login stays
    /// idempotent.
    last_resort: Mutex<HashMap<String, Vec<u8>>>,
    /// Every `welcome.deliver` call, in order.
    welcomes: Mutex<Vec<WelcomeCall>>,
    /// `channel_hex → {actor_hex}` — the nest's `actor_channels` routing
    /// roster. Written on a **successful** `welcome_deliver` exactly as the
    /// real handler does, which is what makes it the phantom-leaf
    /// discriminator: an add whose Welcome never landed leaves an MLS leaf with
    /// no roster row (`mls-group-key-material.md` § M2 *Admitting a member*).
    /// Read back by `channel_actors`.
    roster: Mutex<HashMap<String, std::collections::HashSet<String>>>,
    /// When set, `channel_actors` answers `Ok(None)` — "roster unreadable",
    /// modeling a nest too old to know
    /// `fauna.conversations.channel.actors` (the seam's documented fail-safe).
    roster_unreadable: Mutex<bool>,
    /// The `home_nest_url` of every `channel_actors` seam read, in order —
    /// lets a test assert a foreign-homed read rode the relay param
    /// (`Some(url)` → the `channel.actors_remote` kind on the real seam)
    /// rather than the same-nest path.
    actors_read_urls: Mutex<Vec<Option<String>>>,
    /// When set, `keypackage_upload` stores into this actor's queue. The real
    /// nest infers the uploader from the connection; the seam's
    /// `keypackage_upload(packages)` carries no actor, so the mock is told the
    /// caller explicitly (used by the `ensure_keypackages` test).
    upload_actor_hex: Mutex<Option<String>>,
    /// `bare-localpart → actor_hex`, modeling the nest's `users.handle` column
    /// (which stores bare localparts). Backs `actor_by_handle`.
    handles: Mutex<HashMap<String, String>>,
    /// The nest's handle domain, echoed in every `actor_by_handle` reply (so the
    /// backend can confirm a typed `localpart@domain` targets this nest).
    handle_domain: Mutex<String>,
    /// `(domain, bare-localpart) → (actor_hex, addressable)`, modeling actors on
    /// **foreign** nests reachable via cross-nest `actor_by_handle_remote`.
    remote_handles: Mutex<HashMap<(String, String), (String, bool)>>,
    /// `sealed_cid_hex → sealed blob bytes` — an in-memory model of the nest's
    /// content-addressed blob store (`/api/v1/blob/{cid}`) that conversation
    /// attachments ride. Keyed by the BLAKE3 of the sealed bytes, opaque (the
    /// nest never opens it).
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    /// The `home_nest_url` of every `blob_put` / `blob_get` seam call, in
    /// order — lets a test assert a foreign-homed channel's attachment bytes
    /// were routed to the channel's HOME nest (`Some(url)`) rather than the
    /// member's own (`conversation-rooms.md` § The home nest → *Attachment
    /// bytes*).
    blob_homes: Mutex<Vec<Option<String>>>,
    /// `channel_hex → {seq → legal_reference}` — models the nest's per-record
    /// legal-takedown flag (`segment_records.legal_takedown_ref`): a `channel_fetch`
    /// for a flagged seq serves the sealed envelope **withheld** (empty) and carries
    /// the reference, exactly as `read_after_seq` does (`moderation.md`
    /// § Implementation status today — conversation legal-takedown).
    takedowns: Mutex<HashMap<String, HashMap<i64, String>>>,
    /// `channel_hex → {seq → verdicts}` — what a community room's home nest
    /// serves a live floor member beside a record: the category verdicts of the
    /// labelers the room names (`conversation-rooms.md` § The three classes →
    /// *What the home nest does with its read*, purpose 2).
    served_labels:
        Mutex<HashMap<String, HashMap<i64, Vec<fauna_core::content_category::ContentLabelEntry>>>>,
    /// When set, every `channel_send` fails `Transient` before appending — the
    /// network fault the Rule-1 merge-ordering tests inject (`devices.md`
    /// § Cross-device MLS group-state sync, Durability rules).
    fail_channel_sends: Mutex<bool>,
    /// When set, `welcome_deliver` fails without recording the call — models a
    /// crash/network fault between a commit's `merge_pending_commit` and the
    /// `welcome_deliver` RPC returning:
    /// the group already merged the new leaf, but the Welcome that lived only
    /// in `add_participant`'s local variable never reaches the nest.
    fail_welcome_deliver: Mutex<bool>,
    /// Mirror a nest's short fetch pages (`req.limit.clamp(1, 500)` — so
    /// `limit: 0` returns ONE record, not the whole tail; pages are short
    /// under the frame budget or a non-conforming nest). The paging tests turn
    /// this on to prove the poll fns drain to completion against such a nest;
    /// off, `limit <= 0` returns the full tail per page.
    short_page_clamp: Mutex<bool>,
    /// When set, every `actor_by_handle_remote` fails with this seam error
    /// instead of consulting `remote_handles` — models the peer nest not
    /// answering (`Transient`: DNS / connect / TLS / timeout), refusing
    /// (`Rejected`), or being version-incompatible (`NeedsUpdate`). The
    /// discovery-failure semantics under test: `federation.md` § Peer-auth
    /// model → *Discovery-failure semantics*.
    remote_lookup_fault: Mutex<Option<ConvRpcError>>,
    /// When set, every same-nest `actor_by_handle` fails with this seam error —
    /// the home nest not answering while the user types a recipient.
    home_lookup_fault: Mutex<Option<ConvRpcError>>,
    /// When set, every `actor_by_handle_remote` reply echoes THIS domain rather
    /// than the one dialed — the peer naming a domain it was not reached at.
    /// Models both the hostile case (a nest serving `attacker.test` echoing
    /// `trusted.test`) and the ordinary multi-domain one (a nest reached at a
    /// secondary domain reporting its primary identity domain, which is what a
    /// real nest does when the request carries no `domain` qualifier —
    /// `mail-multidomain.md` § Resolution and login report the live identity
    /// domain).
    remote_echo_domain: Mutex<Option<String>>,
    /// The `peer_domain` argument of every `keypackage_fetch`, in order — lets a
    /// test assert the data plane routes to the domain the client DIALED.
    keypackage_fetch_domains: Mutex<Vec<Option<String>>>,
}

impl MockNest {
    fn seed_keypackage(&self, actor_hex: &str, bytes: Vec<u8>) {
        self.keypackages
            .lock()
            .unwrap()
            .entry(actor_hex.to_string())
            .or_default()
            .push_back(bytes);
    }

    /// Attribute subsequent `keypackage_upload` calls to `actor_hex` (so
    /// `keypackage_count` reflects them, as the real per-actor queue would).
    fn set_upload_actor(&self, actor_hex: &str) {
        *self.upload_actor_hex.lock().unwrap() = Some(actor_hex.to_string());
    }

    /// Register a bare handle → actor mapping and set the nest's handle domain
    /// (`actor_by_handle` echoes the latter). Models a registered user.
    fn seed_handle(&self, localpart: &str, actor_hex: &str, domain: &str) {
        self.handles
            .lock()
            .unwrap()
            .insert(localpart.to_string(), actor_hex.to_string());
        *self.handle_domain.lock().unwrap() = domain.to_string();
    }

    /// Register a handle on a **foreign** nest (`domain` ≠ this nest's) reachable
    /// via `actor_by_handle_remote`, with its `addressable` reachability boolean.
    fn seed_remote_handle(
        &self,
        domain: &str,
        localpart: &str,
        actor_hex: &str,
        addressable: bool,
    ) {
        self.remote_handles.lock().unwrap().insert(
            (domain.to_string(), localpart.to_string()),
            (actor_hex.to_string(), addressable),
        );
    }

    /// Make every subsequent `actor_by_handle_remote` reply echo `domain`
    /// instead of the dialed one. See [`MockNest::remote_echo_domain`].
    fn set_remote_echo_domain(&self, domain: &str) {
        *self.remote_echo_domain.lock().unwrap() = Some(domain.to_string());
    }

    /// The `peer_domain` of every `keypackage_fetch` so far, in call order.
    fn keypackage_fetch_domains(&self) -> Vec<Option<String>> {
        self.keypackage_fetch_domains.lock().unwrap().clone()
    }

    fn keypackage_count_for(&self, actor_hex: &str) -> u64 {
        self.keypackages
            .lock()
            .unwrap()
            .get(actor_hex)
            .map(|q| q.len() as u64)
            .unwrap_or(0)
    }

    /// How many last-resort key packages this actor has on the (mock) nest —
    /// always 0 or 1, since a last-resort upload replaces the prior one.
    fn last_resort_count_for(&self, actor_hex: &str) -> usize {
        usize::from(self.last_resort.lock().unwrap().contains_key(actor_hex))
    }

    /// Take a channel record (1-based `seq`) down under a legal obligation: a
    /// subsequent `channel_fetch` withholds its envelope and carries `reference`,
    /// modeling `fauna.moderation.legal_takedown` on a conversation message.
    fn take_down(&self, channel_hex: &str, seq: i64, reference: &str) {
        self.takedowns
            .lock()
            .unwrap()
            .entry(channel_hex.to_string())
            .or_default()
            .insert(seq, reference.to_string());
    }

    /// Serve `labels` beside the record at `seq` (1-based) — the verdicts a
    /// community room's home nest derived with the labelers the room names.
    fn serve_labels(
        &self,
        channel_hex: &str,
        seq: i64,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    ) {
        self.served_labels
            .lock()
            .unwrap()
            .entry(channel_hex.to_string())
            .or_default()
            .insert(seq, labels);
    }

    /// The plaintext `attachment_refs` of every send on `channel_hex`, in
    /// send order (one list per send; empty for a send with no attachments).
    fn sent_attachment_refs(&self, channel_hex: &str) -> Vec<Vec<String>> {
        self.attachment_refs
            .lock()
            .unwrap()
            .get(channel_hex)
            .cloned()
            .unwrap_or_default()
    }

    fn sent_envelopes(&self, channel_hex: &str) -> Vec<Vec<u8>> {
        self.channels
            .lock()
            .unwrap()
            .get(channel_hex)
            .cloned()
            .unwrap_or_default()
    }

    fn total_sends(&self) -> usize {
        self.channels.lock().unwrap().values().map(Vec::len).sum()
    }

    /// Append a raw envelope to a channel's log **without** going through
    /// `channel_send` — how a test stands in for a party it does not model as
    /// a seat, such as a member sealing bytes an honest client would not
    /// produce. The nest stores what a member posts; judging it is the
    /// reader's job, which is the thing under test.
    fn push_envelope(&self, channel_hex: &str, envelope: Vec<u8>) {
        self.channels
            .lock()
            .unwrap()
            .entry(channel_hex.to_string())
            .or_default()
            .push(envelope);
    }

    fn welcomes(&self) -> Vec<WelcomeCall> {
        self.welcomes.lock().unwrap().clone()
    }

    /// Arm/disarm a hard `Transient` fault on every subsequent `channel_send`.
    fn fail_channel_sends(&self, on: bool) {
        *self.fail_channel_sends.lock().unwrap() = on;
    }

    /// Arm/disarm a hard fault on every subsequent `welcome_deliver` — models
    /// the RPC never landing (crash or transport fault), after any commit it
    /// followed has already merged.
    fn fail_welcome_deliver(&self, on: bool) {
        *self.fail_welcome_deliver.lock().unwrap() = on;
    }

    /// Arm (`Some`) / disarm (`None`) a seam fault on every subsequent
    /// `actor_by_handle_remote` — the peer nest not answering / refusing.
    fn fail_remote_lookups(&self, fault: Option<ConvRpcError>) {
        *self.remote_lookup_fault.lock().unwrap() = fault;
    }

    /// Arm (`Some`) / disarm (`None`) a seam fault on every subsequent same-nest
    /// `actor_by_handle` — the home nest not answering.
    fn fail_home_lookups(&self, fault: Option<ConvRpcError>) {
        *self.home_lookup_fault.lock().unwrap() = fault;
    }

    /// Make `channel_actors` answer "unreadable" (`Ok(None)`) — a transport
    /// failure, or a foreign member whose roster row lives
    /// on their own home nest.
    fn make_roster_unreadable(&self, on: bool) {
        *self.roster_unreadable.lock().unwrap() = on;
    }

    /// Make `channel_fetch` clamp `limit` like a nest serving short pages
    /// (`limit.clamp(1, 500)` — `conversations_handlers.rs`), so a
    /// `limit: 0` request returns ONE record instead of the whole tail.
    fn clamp_limit_to_short_pages(&self, on: bool) {
        *self.short_page_clamp.lock().unwrap() = on;
    }
}

#[async_trait]
impl ConversationsRpc for MockNest {
    async fn channel_send_remote(
        &self,
        _channel_id_hex: String,
        _home_nest_url: String,
        _envelope: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        // Single-nest mock — reaching the remote path is a routing bug.
        Err(ConvRpcError::Rejected {
            message: "mock nest has no federation relay (unexpected send_remote)".into(),
        })
    }

    async fn channel_send(
        &self,
        channel_id_hex: String,
        envelope: Vec<u8>,
        // The device-owned-epoch gate is exercised end-to-end in the tier_3
        // `test_mls_replica_sync.py` (real nest); this in-process mock is a blind
        // append, so the precondition is recorded-but-not-enforced.
        _expect_no_commit_since: Option<i64>,
        attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        if *self.fail_channel_sends.lock().unwrap() {
            return Err(ConvRpcError::Transient {
                message: "injected channel_send fault".into(),
            });
        }
        self.attachment_refs
            .lock()
            .unwrap()
            .entry(channel_id_hex.clone())
            .or_default()
            .push(attachment_refs);
        let mut chans = self.channels.lock().unwrap();
        let log = chans.entry(channel_id_hex).or_default();
        log.push(envelope);
        Ok(log.len() as i64)
    }

    async fn channel_fetch(
        &self,
        channel_id_hex: String,
        after: i64,
        limit: i64,
        // Same-nest mock: the relay home URL is irrelevant (no foreign nest).
        _home_nest_url: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        use fauna_conversations::backend::FetchedRecord;
        let chans = self.channels.lock().unwrap();
        let takedowns = self.takedowns.lock().unwrap();
        let taken = takedowns.get(&channel_id_hex);
        let served_labels = self.served_labels.lock().unwrap();
        let labelled = served_labels.get(&channel_id_hex);
        let Some(log) = chans.get(&channel_id_hex) else {
            return Ok(vec![]);
        };
        // A nest serving short pages clamps `limit.clamp(1, 500)`, so `limit: 0`
        // yields a ONE-record page (the Bug-A mismatch the paging tests pin);
        // otherwise `limit <= 0` is the full page.
        let effective_limit = if *self.short_page_clamp.lock().unwrap() {
            limit.clamp(1, 500)
        } else {
            limit
        };
        let mut out = Vec::new();
        for (idx, env) in log.iter().enumerate() {
            let seq = (idx + 1) as i64;
            if seq > after {
                // A taken-down record: withhold the sealed envelope (empty) and carry
                // the legal reference, exactly as the nest's `read_after_seq` gate does.
                match taken.and_then(|t| t.get(&seq)) {
                    Some(reference) => out.push(FetchedRecord {
                        seq,
                        legal_takedown_ref: Some(reference.clone()),
                        ..Default::default()
                    }),
                    // A community room's verdicts ride beside the envelope, as
                    // the home nest serves them to a live floor member.
                    None => out.push(FetchedRecord {
                        seq,
                        envelope: env.clone(),
                        labels: labelled
                            .and_then(|l| l.get(&seq))
                            .cloned()
                            .unwrap_or_default(),
                        ..Default::default()
                    }),
                }
                if effective_limit > 0 && out.len() as i64 >= effective_limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    async fn keypackage_count(&self, actor_id_hex: String) -> Result<u64, ConvRpcError> {
        Ok(self
            .keypackages
            .lock()
            .unwrap()
            .get(&actor_id_hex)
            .map(|q| q.len() as u64)
            .unwrap_or(0))
    }

    async fn actor_by_handle(
        &self,
        handle: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        if let Some(fault) = self.home_lookup_fault.lock().unwrap().clone() {
            return Err(fault);
        }
        Ok(self.handles.lock().unwrap().get(&handle).map(|actor_hex| {
            fauna_conversations::backend::ResolvedHandle {
                actor_id_hex: actor_hex.clone(),
                echoed_domain: self.handle_domain.lock().unwrap().clone(),
                // Same-nest reachability is decided by `resolve_reachable`
                // (keypackage_count); mirror it here for an honest reply.
                addressable: self.keypackage_count_for(actor_hex) > 0,
            }
        }))
    }

    async fn actor_by_handle_remote(
        &self,
        domain: String,
        localpart: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        if let Some(fault) = self.remote_lookup_fault.lock().unwrap().clone() {
            return Err(fault);
        }
        let echoed = self
            .remote_echo_domain
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| domain.clone());
        Ok(self
            .remote_handles
            .lock()
            .unwrap()
            .get(&(domain, localpart))
            .map(
                |(actor_hex, addressable)| fauna_conversations::backend::ResolvedHandle {
                    actor_id_hex: actor_hex.clone(),
                    echoed_domain: echoed,
                    addressable: *addressable,
                },
            ))
    }

    async fn keypackage_fetch(
        &self,
        actor_id_hex: String,
        peer_domain: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        self.keypackage_fetch_domains
            .lock()
            .unwrap()
            .push(peer_domain);
        Ok(self
            .keypackages
            .lock()
            .unwrap()
            .get_mut(&actor_id_hex)
            .and_then(|q| q.pop_front()))
    }

    async fn keypackage_upload(
        &self,
        packages: Vec<Vec<u8>>,
        last_resort: bool,
    ) -> Result<u64, ConvRpcError> {
        if let Some(actor) = self.upload_actor_hex.lock().unwrap().clone() {
            if last_resort {
                // One reusable last-resort KP per actor: REPLACE, never append
                // (the nest keeps a single last-resort row per actor).
                let mut lr = self.last_resort.lock().unwrap();
                if let Some(p) = packages.last() {
                    lr.insert(actor, p.clone());
                }
            } else {
                let mut kps = self.keypackages.lock().unwrap();
                let q = kps.entry(actor).or_default();
                for p in &packages {
                    q.push_back(p.clone());
                }
            }
        }
        Ok(packages.len() as u64)
    }

    async fn welcome_deliver(
        &self,
        recipient_actor_id_hex: String,
        channel_id_hex: String,
        welcome_bytes: Vec<u8>,
        kind: WelcomeChannelKind,
        peer_domain: Option<String>,
    ) -> Result<(), ConvRpcError> {
        if *self.fail_welcome_deliver.lock().unwrap() {
            return Err(ConvRpcError::Transient {
                message: "injected welcome_deliver fault".into(),
            });
        }
        // The real `welcome.deliver` handler registers the recipient on the
        // channel's `actor_channels` roster as part of delivering. Modeling
        // that here is what gives the mock a truthful discriminator: the
        // fault above returns *before* this point, so an injected failure
        // leaves the leaf rostered nowhere.
        self.roster
            .lock()
            .unwrap()
            .entry(channel_id_hex.clone())
            .or_default()
            .insert(recipient_actor_id_hex.to_ascii_lowercase());
        self.welcomes.lock().unwrap().push(WelcomeCall {
            recipient_hex: recipient_actor_id_hex,
            channel_hex: channel_id_hex,
            welcome_bytes,
            kind,
            peer_domain,
        });
        Ok(())
    }

    async fn channel_actors(
        &self,
        channel_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<Option<Vec<String>>, ConvRpcError> {
        self.actors_read_urls
            .lock()
            .unwrap()
            .push(home_nest_url.clone());
        if *self.roster_unreadable.lock().unwrap() {
            return Ok(None);
        }
        // One roster either way: same-nest reads it directly; a `Some(url)`
        // read models the home nest answering its union over the
        // `channel.actors_remote` → `fauna.federation.channel.actors` relay.
        Ok(Some(
            self.roster
                .lock()
                .unwrap()
                .get(&channel_id_hex)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default(),
        ))
    }

    async fn blob_put(
        &self,
        _channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
        bytes: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        // Faithful to the real content-addressed store: the key IS the BLAKE3 of
        // the stored (sealed) bytes, so a mismatched cid is a caller bug.
        assert_eq!(
            sealed_cid_hex,
            hex::encode(blake3::hash(&bytes).as_bytes()),
            "blob_put sealed_cid must be the BLAKE3 of the bytes"
        );
        self.blob_homes.lock().unwrap().push(home_nest_url);
        self.blobs.lock().unwrap().insert(sealed_cid_hex, bytes);
        Ok(())
    }

    async fn blob_get(
        &self,
        _channel_id_hex: String,
        home_nest_url: Option<String>,
        sealed_cid_hex: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        self.blob_homes.lock().unwrap().push(home_nest_url);
        Ok(self.blobs.lock().unwrap().get(&sealed_cid_hex).cloned())
    }
}

// ── Dim 4: a version/schema mismatch surfaces as the distinct, non-retry
// `BackendError::NeedsUpdate`, never a flattened `Transport(raw)` ──────────
//
// `version-compatibility.md` Dimension 4 / § 5 item 3 — the named `backend.rs`
// leak: the conversations rail used to flatten every seam failure to
// `BackendError::Transport(raw_details)`, so a degraded nest's
// `fauna.nest.outdated` reached the UI as a raw transport string instead of a
// localized, actionable "update your nest" prompt. The glue
// (`fauna-client-conversations`) builds `ConvRpcError::NeedsUpdate` from the
// shared `RpcError::action()` / `localized()` classifier (the wire-code → class
// half, unit-tested in that crate); this proves the `fauna-conversations` half —
// that the seam's `NeedsUpdate` is preserved as the distinct, localized
// `BackendError::NeedsUpdate` (non-retry), not collapsed back to `Transport`.

const OUTDATED_MSG: &str = "This nest is running an outdated version and must be updated.";

fn outdated_seam_error() -> ConvRpcError {
    ConvRpcError::NeedsUpdate {
        message: OUTDATED_MSG.to_string(),
    }
}

/// A degraded (outdated) nest: every seam call answers with the version-mismatch
/// classification, exactly as the glue would for a `fauna.nest.outdated` reply.
struct OutdatedNest;

#[async_trait]
impl ConversationsRpc for OutdatedNest {
    async fn channel_send(
        &self,
        _c: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn channel_send_remote(
        &self,
        _c: String,
        _u: String,
        _e: Vec<u8>,
        _expect_no_commit_since: Option<i64>,
        _attachment_refs: Vec<String>,
    ) -> Result<i64, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn channel_fetch(
        &self,
        _c: String,
        _a: i64,
        _l: i64,
        _h: Option<String>,
    ) -> Result<Vec<fauna_conversations::backend::FetchedRecord>, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn keypackage_count(&self, _a: String) -> Result<u64, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn actor_by_handle(
        &self,
        _h: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn actor_by_handle_remote(
        &self,
        _d: String,
        _l: String,
    ) -> Result<Option<fauna_conversations::backend::ResolvedHandle>, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn keypackage_fetch(
        &self,
        _a: String,
        _p: Option<String>,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn keypackage_upload(&self, _p: Vec<Vec<u8>>, _l: bool) -> Result<u64, ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn welcome_deliver(
        &self,
        _r: String,
        _c: String,
        _w: Vec<u8>,
        _k: WelcomeChannelKind,
        _p: Option<String>,
    ) -> Result<(), ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn blob_put(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
        _b: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        Err(outdated_seam_error())
    }
    async fn blob_get(
        &self,
        _c: String,
        _h: Option<String>,
        _s: String,
    ) -> Result<Option<Vec<u8>>, ConvRpcError> {
        Err(outdated_seam_error())
    }
}

#[tokio::test]
async fn outdated_nest_seam_error_surfaces_as_backend_needs_update() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = MlsEngine::new_in_memory(ActorKeypair::generate())
        .unwrap()
        .identity_actor_id();
    // `deliver_scheduling_imip` hits the seam's `keypackage_fetch` first, before
    // any MLS crypto — so the degraded-nest error is what surfaces.
    let backend = FaunaMlsBackend::new(alice, Arc::new(OutdatedNest), "alice", alice_actor);

    let err = backend
        .deliver_scheduling_imip(bob_actor, None, b"BEGIN:VCALENDAR\r\n".to_vec())
        .await
        .expect_err("a degraded nest must fail the delivery");

    match err {
        BackendError::NeedsUpdate { message } => assert_eq!(
            message, OUTDATED_MSG,
            "the localized update message must be preserved verbatim through the seam"
        ),
        other => panic!(
            "expected BackendError::NeedsUpdate (non-retry, localized); got {other:?} \
             — the version-mismatch classification was lost / flattened to Transport"
        ),
    }
}

#[test]
fn engine_accessor_hands_out_the_same_shared_handle() {
    // The folder author seam (`FoldersAuthor`, via the `FolderGroupCrypto`
    // adapter on `Arc<MlsEngine>`) reuses the conversations rail's ONE per-actor
    // `MlsEngine` over the ONE `mls_state.db` — never a second engine racing on the
    // SQLite file (`apps/fauna-linux/src/mls.rs`). So the accessor MUST hand out the
    // SAME `Arc` the backend/session was built with, not a clone of a fresh engine.
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let backend = FaunaMlsBackend::new(Arc::clone(&engine), Arc::new(OutdatedNest), "alice", actor);
    assert!(
        Arc::ptr_eq(&engine, &backend.engine()),
        "FaunaMlsBackend::engine must return the same engine handle"
    );

    let session = ConversationsSession::from_parts(
        Arc::clone(&engine),
        Arc::new(OutdatedNest),
        "alice".into(),
        actor,
        None,
    );
    assert!(
        Arc::ptr_eq(&engine, &session.engine()),
        "ConversationsSession::engine must delegate to the same shared handle"
    );
}

#[test]
fn conv_rpc_error_maps_onto_backend_error_by_class() {
    // The seam's three-way classification (mirroring `RpcErrorAction`) collapses
    // onto `BackendError` preserving only the *new* version-mismatch routing:
    // NeedsUpdate keeps its own non-retry variant; Rejected/Transient reuse the
    // pre-existing show-message / retry paths.
    assert!(matches!(
        BackendError::from(ConvRpcError::NeedsUpdate { message: "u".into() }),
        BackendError::NeedsUpdate { message } if message == "u"
    ));
    assert!(matches!(
        BackendError::from(ConvRpcError::Rejected { message: "r".into() }),
        BackendError::Refusal(m) if m == "r"
    ));
    assert!(matches!(
        BackendError::from(ConvRpcError::Transient { message: "t".into() }),
        BackendError::Transport(m) if m.as_str() == "t"
    ));
}

#[test]
fn user_detail_maps_every_variant_to_user_renderable_text() {
    // The send-slot taxonomy (conversations.md § Errors & edge cases, ratified
    // 2026-08-02): product statements pass through verbatim; conditions map to
    // their i18n sentence; a diagnostic payload NEVER reaches the slot.
    use fauna_i18n::strings::error::send;
    assert_eq!(
        BackendError::NeedsUpdate {
            message: "u".into()
        }
        .user_detail(),
        "u"
    );
    assert_eq!(
        BackendError::Refusal("refused".into()).user_detail(),
        "refused"
    );
    assert_eq!(
        BackendError::transport_from_seam("seam sentence").user_detail(),
        "seam sentence"
    );
    assert_eq!(
        BackendError::AuthRequired.user_detail(),
        send::AUTH_REQUIRED
    );
    assert_eq!(
        BackendError::NotSupported.user_detail(),
        send::NOT_SUPPORTED
    );
    let internal = BackendError::Internal("no key package available for abc".into());
    assert_eq!(internal.user_detail(), send::GENERIC);
    assert!(
        !internal.user_detail().contains("key package"),
        "a diagnostic payload must never leak into the user-renderable detail"
    );
}

/// An **MLS-engine** failure must reach the send slot as the generic sentence,
/// never as its own diagnostic text.
///
/// The sibling pins above check `user_detail()` variant-by-variant; they cannot
/// see a *producer* that classified its error into the wrong variant in the
/// first place, which is exactly how the seven `fauna_mls.rs` engine sites
/// leaked. So this one drives a real engine failure end-to-end through the
/// manager and reads the slot the apps render — the only vantage point from
/// which "a raw openmls string reached `error-message`" is observable.
///
/// The failure is a genuine production shape, not a stub: a thread bound to a
/// channel whose group this engine does not hold (the state a device lands in
/// when its engine store was rebuilt but the thread binding survived) makes
/// `MlsEngine::encrypt` return `ChannelNotFound`, whose payload is the raw
/// channel hex.
///
/// `conversations.md` § Errors & edge cases (send-slot taxonomy, ratified
/// 2026-08-02): diagnostics ride `Internal`, whose payload is never rendered.
#[tokio::test]
async fn an_engine_failure_reaches_the_send_slot_as_the_generic_sentence() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    let manager = ConversationsManager::new();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    manager.register_backend(backend.clone());

    // Materialize a thread the ordinary way (an inbound message), then bind it
    // to a channel this engine holds no group for.
    let bob_actor = ActorId([9u8; 32]);
    manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("bob", bob_actor),
            recipients: vec![fauna_addr("alice", alice_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let thread_id = manager.snapshot().threads[0].thread_id.clone();
    let orphan_channel = ChannelId::from_hex(&"ab".repeat(32)).expect("valid channel hex");
    backend.bind_channel(thread_id.clone(), orphan_channel);

    manager.select_thread(thread_id.clone());
    manager.set_compose_body(thread_id.clone(), "this send hits a missing group".into());
    let err = manager
        .send(thread_id.clone())
        .await
        .expect_err("encrypting on a channel with no group must fail");

    // Classification: an engine fault is a diagnostic.
    assert!(
        matches!(err, BackendError::Internal(_)),
        "an MLS-engine failure is a diagnostic and must ride Internal, got {err:?}"
    );

    // And the slot the 7 apps render carries the generic sentence — not the
    // engine's own words.
    let detail = manager.thread_detail(thread_id).expect("thread exists");
    match &detail.compose.send_state {
        SendState::Failed { reason } => {
            assert_eq!(reason.key, "conversations.unified.error_send");
            let message = reason
                .args
                .get("message")
                .expect("the backend detail rides {message}");
            assert_eq!(
                message,
                fauna_i18n::strings::error::send::GENERIC,
                "an engine diagnostic must be replaced by the generic sentence, not rendered"
            );
            // Named substrings, so this still reds if the generic sentence is
            // ever reworded into something that happens to contain them.
            let channel_hex = orphan_channel.to_string();
            assert!(
                !message.contains(&channel_hex) && !message.to_lowercase().contains("channel"),
                "the raw engine diagnostic leaked into the user-rendered slot: {message:?}"
            );
        }
        other => panic!("expected send_state == Failed after a failed send, got {other:?}"),
    }
}

/// The channel-keyed thread a restored device starts every commit-walk test
/// with: **no messages**, one named participant, nothing else known. Nine
/// tests need exactly this and used to hand-list all ten slice fields each, so
/// a new field meant nine edits — and a merge conflict for every branch that
/// grew the slice concurrently. The restore is how these tests get a thread bound to
/// `channel_hex` without a Welcome.
fn empty_channel_slice(
    channel_hex: &str,
    label: &str,
    participant: TypedAddress,
) -> ChannelHistorySlice {
    ChannelHistorySlice {
        channel_id_hex: channel_hex.to_string(),
        label: label.to_string(),
        participants: vec![participant],
        ..Default::default()
    }
}

fn fauna_addr(handle: &str, actor: ActorId) -> TypedAddress {
    TypedAddress::Fauna {
        handle: handle.into(),
        actor_id: actor,
    }
}

fn fauna_mls_thread(thread_id: ThreadId, participants: Vec<TypedAddress>) -> ThreadDetail {
    let flavor = if participants.len() <= 1 {
        ThreadFlavor::OneToOne
    } else {
        ThreadFlavor::MlsGroup
    };
    ThreadDetail {
        thread_id,
        rail: Rail::FaunaMls,
        glyph: Rail::FaunaMls.glyph(),
        flavor: flavor.clone(),
        label: "thread".into(),
        participant_displays: participants.iter().map(|p| p.display()).collect(),
        participants,
        capabilities: derive_capabilities(Rail::FaunaMls, flavor),
        messages: vec![],
        compose: ComposeState::default(),
        selected_message_id: None,
        bridge: None,
        guardian_state: None,
        room: None,
    }
}

// ── Track A: send on a channel already bound to its MLS group ──────────

#[tokio::test]
async fn send_encrypts_and_posts_envelope_that_peer_decrypts() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel_id = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel_id);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    let thread_id = ThreadId("t-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    let thread = fauna_mls_thread(thread_id, vec![]);
    let compose = ComposeState {
        body_draft: "ping".into(),
        ..Default::default()
    };

    let outcome = backend.send(&thread, &compose, &[]).await.expect("send ok");

    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(
        envelopes.len(),
        1,
        "exactly one channel.send to the channel"
    );
    assert_eq!(nest.total_sends(), 1);

    let env = ChannelEnvelope::from_bytes(&envelopes[0]).expect("decode ChannelEnvelope");
    let mls_bytes = match env {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application envelope"),
    };
    let decrypted = bob.decrypt(&channel_id, &mls_bytes).expect("peer decrypts");
    match decrypted.body {
        ChannelMessageBody::Text(t) => assert_eq!(t, "ping"),
        other => panic!("expected Text body, got {other:?}"),
    }
    assert_eq!(decrypted.sender, alice.identity_actor_id());
    assert!(matches!(outcome.sender, TypedAddress::Fauna { .. }));
}

#[tokio::test]
async fn send_unbound_thread_with_no_peers_errors() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);

    // No binding AND no peers to bootstrap a group with → must error, not panic.
    let thread = fauna_mls_thread(ThreadId("t-unbound".into()), vec![]);
    let err = backend
        .send(&thread, &ComposeState::default(), &[])
        .await
        .expect_err("unbound thread with no peers must error");
    assert!(matches!(err, BackendError::Internal(_)), "got {err:?}");
    assert_eq!(nest.total_sends(), 0, "nothing posted");
}

// ── Track B: lazy group bootstrap on an unbound new thread ─────────────

#[tokio::test]
async fn send_bootstraps_group_when_unbound() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    // Bob publishes a key package to the nest (the keypackage.upload shape).
    let nest = Arc::new(MockNest::default());
    let bob_kp_bytes = bob.generate_key_packages_bytes(1).unwrap();
    nest.seed_keypackage(&bob_hex, bob_kp_bytes[0].clone());

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    // Unbound new 1:1 thread with Bob as the peer.
    let thread = fauna_mls_thread(ThreadId("t-1".into()), vec![fauna_addr("bob", bob_actor)]);
    let compose = ComposeState {
        body_draft: "ping".into(),
        ..Default::default()
    };

    backend
        .send(&thread, &compose, &[])
        .await
        .expect("bootstrap+send ok");

    // A welcome was delivered to Bob for the new channel (1:1 ⇒ not a group).
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "one welcome delivered");
    assert_eq!(welcomes[0].recipient_hex, bob_hex);
    assert_eq!(
        welcomes[0].kind,
        WelcomeChannelKind::Dm,
        "1:1 bootstrap is a DM welcome (no group id)"
    );
    let channel_hex = welcomes[0].channel_hex.clone();

    // Exactly one ciphertext was posted to the new channel, and Bob — joining
    // from the delivered welcome — decrypts it back to the original text.
    assert_eq!(nest.total_sends(), 1);
    let bob_channel = bob
        .join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .expect("bob joins from welcome");
    assert_eq!(bob_channel.to_string(), channel_hex);
    let envelopes = nest.sent_envelopes(&channel_hex);
    let mls_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    let decrypted = bob.decrypt(&bob_channel, &mls_bytes).expect("bob decrypts");
    assert!(matches!(decrypted.body, ChannelMessageBody::Text(t) if t == "ping"));

    // The thread is now bound: a second send reuses the channel (no new group,
    // no new welcome).
    let compose2 = ComposeState {
        body_draft: "again".into(),
        ..Default::default()
    };
    backend
        .send(&thread, &compose2, &[])
        .await
        .expect("second send ok");
    assert_eq!(
        nest.welcomes().len(),
        1,
        "no second welcome — binding reused"
    );
    assert_eq!(
        nest.total_sends(),
        2,
        "second ciphertext on the same channel"
    );
}

// ── Slice 3-core: mailbox-less CalDAV scheduling iMIP delivery ─────────
// The WS-RPC sealed-delivery rail (caldav-server.md § Server-side
// auto-schedule, Half-1): an organizer delivers an iMIP to a mailbox-less
// Fauna attendee (CalDAV on / email off) over a fresh one-off MLS channel.

#[tokio::test]
async fn deliver_scheduling_imip_creates_oneoff_group_and_posts_imip() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    // Bob has published a key package — the only standing surface relied on, the
    // very same one the chat rail fetches (no new standing-key surface).
    let nest = Arc::new(MockNest::default());
    let bob_kp_bytes = bob.generate_key_packages_bytes(1).unwrap();
    nest.seed_keypackage(&bob_hex, bob_kp_bytes[0].clone());

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    // A representative iMIP REQUEST exactly as the email rail would carry it
    // (raw RFC 5322 with a text/calendar part) — opaque to the sender, carried
    // verbatim so the recipient extracts it identically on either transport.
    let imip: Vec<u8> = b"From: alice@nest.test\r\nTo: bob@nest.test\r\n\
Content-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n\
BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-1\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        .to_vec();

    backend
        .deliver_scheduling_imip(bob_actor, None, imip.clone())
        .await
        .expect("scheduling delivery ok");

    // A single Welcome was delivered to Bob, tagged Scheduling — the receive
    // loop routes the channel to calendar-apply, never the chat UI.
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "one scheduling welcome delivered");
    assert_eq!(welcomes[0].recipient_hex, bob_hex);
    assert_eq!(
        welcomes[0].kind,
        WelcomeChannelKind::Scheduling,
        "a mailbox-less iMIP rides a Scheduling welcome"
    );
    assert_eq!(
        welcomes[0].peer_domain, None,
        "same-nest recipient → no relay"
    );
    let channel_hex = welcomes[0].channel_hex.clone();

    // Exactly one application message was posted; Bob — joining from the
    // delivered welcome — decrypts it back to the iMIP bytes as a Scheduling
    // body (NOT a chat Text bubble).
    assert_eq!(nest.total_sends(), 1, "one app message posted");
    let bob_channel = bob
        .join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .expect("bob joins from the scheduling welcome");
    assert_eq!(bob_channel.to_string(), channel_hex);
    let envelopes = nest.sent_envelopes(&channel_hex);
    let mls_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    let decrypted = bob.decrypt(&bob_channel, &mls_bytes).expect("bob decrypts");
    match decrypted.body {
        ChannelMessageBody::Scheduling(bytes) => assert_eq!(bytes, imip),
        other => panic!("expected a Scheduling body carrying the iMIP, got {other:?}"),
    }
    assert_eq!(decrypted.sender, alice.identity_actor_id());

    // The delivery binds no thread — one-off, never a chat conversation (the
    // organizer never receives on this channel; an RSVP comes back as its own
    // dispatch).
    assert!(
        backend.bound_channels().is_empty(),
        "a scheduling delivery binds no chat thread"
    );
}

// ── Slice 4: recipient receive-loop scheduling DRAIN ───────────────────
// The mailbox-less recipient side of the WS-RPC rail: a `Scheduling` welcome
// joins a one-off channel WITHOUT materializing a chat thread, and the channel's
// iMIP application message drains to the calendar-apply `SchedulingSink` — never
// the conversation UI (caldav-server.md § Server-side auto-schedule, Half-1).

/// Records every iMIP handed to the calendar-apply sink, in order — the test
/// twin of the real `NestSchedulingSink` (which routes to `CalDavClient`).
#[derive(Default)]
struct MockSchedulingSink {
    applied: Mutex<Vec<Vec<u8>>>,
}

#[async_trait]
impl SchedulingSink for MockSchedulingSink {
    async fn apply_scheduling_imip(
        &self,
        raw_rfc5322: Vec<u8>,
        _origin: fauna_conversations::backend::SchedulingOrigin,
    ) -> Result<(), String> {
        self.applied.lock().unwrap().push(raw_rfc5322);
        Ok(())
    }
}

/// A representative iMIP REQUEST exactly as the email rail carries it (raw RFC
/// 5322 with a `text/calendar` part) — the same bytes the sender posts and the
/// drain hands to the sink verbatim.
fn sample_imip() -> Vec<u8> {
    b"From: alice@nest.test\r\nTo: bob@nest.test\r\n\
Content-Type: text/calendar; method=REQUEST; charset=utf-8\r\n\r\n\
BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:evt-drain-1\r\n\
END:VEVENT\r\nEND:VCALENDAR\r\n"
        .to_vec()
}

#[tokio::test]
async fn scheduling_welcome_ingest_and_drain_routes_imip_to_sink() {
    // Alice (organizer) delivers a scheduling iMIP to mailbox-less Bob over a
    // one-off channel — the Slice 3-core sender.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    let imip = sample_imip();
    alice_backend
        .deliver_scheduling_imip(bob_actor, None, imip.clone())
        .await
        .expect("scheduling delivery ok");
    let welcome = nest.welcomes()[0].clone();

    // Bob's backend over the same nest ingests the Scheduling welcome.
    let bob_backend = FaunaMlsBackend::new(bob, nest.clone(), "bob", bob_actor);
    let channel = ingest_scheduling_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("ingest scheduling welcome");

    // A scheduling welcome marks the channel scheduling and binds NO chat thread.
    assert!(bob_backend.is_scheduling_channel(&channel));
    assert_eq!(bob_backend.scheduling_channels(), vec![channel]);
    assert!(
        bob_backend.bound_channels().is_empty(),
        "a scheduling delivery never materializes a chat thread"
    );

    // Draining the channel hands the exact iMIP bytes to the sink (decrypted back
    // from the `Scheduling` body), never a chat ingest.
    let sink = MockSchedulingSink::default();
    let mut after = 0i64;
    let applied = poll_inbound_scheduling(&bob_backend, &sink, &channel, &mut after, 0)
        .await
        .expect("drain ok");
    assert_eq!(applied, 1, "one iMIP applied");
    assert_eq!(sink.applied.lock().unwrap().as_slice(), &[imip]);

    // Idempotent: a re-drain with the advanced cursor applies nothing more.
    let again = poll_inbound_scheduling(&bob_backend, &sink, &channel, &mut after, 0)
        .await
        .expect("re-drain ok");
    assert_eq!(again, 0, "cursor dedups — no double-apply");
    assert_eq!(sink.applied.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn scheduling_welcome_ingest_is_idempotent() {
    // A re-delivered scheduling welcome for an already-joined channel must not
    // attempt a second (init-key-spending) join — it returns the same channel.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    alice_backend
        .deliver_scheduling_imip(bob_actor, None, sample_imip())
        .await
        .expect("scheduling delivery ok");
    let welcome = nest.welcomes()[0].clone();

    let bob_backend = FaunaMlsBackend::new(bob, nest.clone(), "bob", bob_actor);
    let c1 = ingest_scheduling_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("first ingest");
    let c2 = ingest_scheduling_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("second ingest is a no-op join");
    assert_eq!(c1, c2);
    assert_eq!(bob_backend.scheduling_channels(), vec![c1]);
}

#[tokio::test]
async fn receive_loop_drains_scheduling_welcome_to_sink_without_a_thread() {
    // End-to-end through the detached receive loop: a `Scheduling` welcome pushed
    // on the loop drains to the registered sink and surfaces NO chat thread.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    let imip = sample_imip();
    alice_backend
        .deliver_scheduling_imip(bob_actor, None, imip.clone())
        .await
        .expect("scheduling delivery ok");
    let welcome = nest.welcomes()[0].clone();

    // Bob's session: a push source seeded with the one Scheduling welcome, plus a
    // registered calendar-apply sink.
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::from([ConvPushEvent::Welcome(WelcomeNudge {
            channel_id_hex: Some(welcome.channel_hex.clone()),
            welcome_bytes: welcome.welcome_bytes.clone(),
            kind: WelcomeChannelKind::Scheduling,
            home_nest_url: None,
            shared_by: None,
            set_name: None,
            access: None,
            home_nest_actor_id: None,
            set_name_seal: None,
            shared_by_handle: None,
            shared_by_domain: None,
        })])),
    });
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, Some(push));
    let sink = Arc::new(MockSchedulingSink::default());
    session.register_scheduling_sink(sink.clone());
    let manager = session.manager();

    session.start_receive_loop().await;

    // Bounded wait for the loop to ingest the welcome + drain the iMIP to the sink.
    let mut got = false;
    for _ in 0..200 {
        if sink.applied.lock().unwrap().len() == 1 {
            got = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(got, "receive loop drained the scheduling iMIP to the sink");
    assert_eq!(sink.applied.lock().unwrap().as_slice(), &[imip]);
    assert!(
        manager.snapshot().threads.is_empty(),
        "a scheduling delivery surfaces no conversation thread"
    );
}

// ── Track B: inbound decrypt driver routes into the bound thread ───────

/// **A retired engine must not WALK the inbound log**, and the refusal has to
/// land before the cursor moves.
///
/// The walk advances `after_seq` past a record *before* decrypting it, and
/// skips every decrypt failure with `continue` — correctly, since a sender
/// cannot decrypt its own application messages. So an engine whose `decrypt`
/// merely started failing typed would silently skip every record it walked,
/// and the session writes that advanced cursor to the durable cross-device
/// watermark: strictly worse than the ghost it replaces, because the skip
/// becomes permanent and reaches the identity's other devices.
///
/// Refusing at the entry leaves the records on the nest log for the successor,
/// which holds the retire-point snapshot and can still decrypt
/// them.
#[tokio::test]
async fn a_retired_engine_refuses_the_inbound_walk_without_moving_the_cursor() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend.bind_channel(ThreadId("a-1".into()), channel_id);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
            &ComposeState {
                body_draft: "a message the ghost must not consume".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice send ok");

    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // The role is handed over while this backend is still reachable — the
    // failed-successor-build strand, and the success path's window.
    bob.retire();

    let mut after_seq = 0i64;
    let outcome =
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0).await;

    assert!(
        outcome.is_err(),
        "a retired engine walked the inbound log — every record it passes is one \
         its successor will never see"
    );
    assert_eq!(
        after_seq, 0,
        "THE assertion: the cursor must not move. An advanced cursor reaches the \
         durable cross-device watermark, so a skip here is permanent and travels \
         to the identity's other devices"
    );
}

#[tokio::test]
async fn inbound_driver_routes_decrypted_message_to_bound_thread() {
    // Alice creates the group and sends "ping" through the seam.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let alice_thread = ThreadId("a-1".into());
    alice_backend.bind_channel(alice_thread, channel_id);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice send ok");

    // Bob's side: a manager with the FaunaMls backend, a thread bound to the
    // channel (in production created by the welcome-ingest, Track C; here seeded
    // via a setup ingest so the test needs no test-helpers feature).
    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Seed Bob's thread (participant-keyed) and bind it to the channel.
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    // Drive the inbound feed: fetch → decode envelope → MLS-decrypt → ingest.
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(ingested, 1, "one inbound message ingested");
    assert!(after_seq >= 1, "cursor advanced past the fetched message");

    // The decrypted "ping" landed in Bob's bound thread.
    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    assert!(
        detail.messages.iter().any(|m| m.body == "ping"),
        "decrypted message routed into the bound thread; got {:?}",
        detail.messages.iter().map(|m| &m.body).collect::<Vec<_>>()
    );

    // A second poll with the advanced cursor ingests nothing (no double-count).
    let again = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(again, 0, "cursor prevents re-ingest");
}

/// **The record-level skip for `ChannelMessageBody`, pinned at the loop**
/// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in full*:
/// the enum is ruled skip; `archive-import.md` names this the precondition for
/// a new body variant — released before the variant exists). A member on a
/// LATER build posts a top-level body this build has no variant for: that one
/// record fails its decode and is skipped — not fatal, nothing ingested for
/// it, the walk not stalled — and the next ordinary message still lands. The
/// nested `GroupMetaMessage` case is pinned in `fauna-mls::types`; this is the
/// top-level case, through `poll_inbound_conv` itself.
#[tokio::test]
async fn an_unknown_top_level_body_skips_only_its_record_in_the_inbound_walk() {
    use serde::Serialize;

    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();

    // A later build's body enum: the same wire shape as `ChannelMessage`, with
    // a top-level variant this build has never heard of (external tagging makes
    // the variant name the map key, so this is byte-identical to that build's).
    #[derive(Serialize)]
    enum LaterBody {
        Hologram { frames: u32 },
    }
    #[derive(Serialize)]
    struct LaterChannelMessage {
        sender: ActorId,
        sequence: u64,
        channel_epoch: u64,
        body: LaterBody,
        timestamp: Timestamp,
    }
    let later = fauna_cbor::encode_canonical(&LaterChannelMessage {
        sender: alice_actor,
        sequence: 1,
        channel_epoch: 0,
        body: LaterBody::Hologram { frames: 24 },
        timestamp: Timestamp(1_700_000_000_000_000),
    })
    .unwrap();
    assert!(
        fauna_cbor::decode_strict::<ChannelMessage>(&later).is_err(),
        "the later body must not decode here — or this pins nothing"
    );
    let sealed = alice
        .encrypt_payload_for_test(&channel_id, &later)
        .expect("seal the later build's payload");
    nest.channels
        .lock()
        .unwrap()
        .entry(channel_id.to_string())
        .or_default()
        .push(ChannelEnvelope::Application(sealed).to_bytes().unwrap());

    // Then an ordinary message from this build, after it in the log.
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend.bind_channel(ThreadId("a-1".into()), channel_id);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
            &ComposeState {
                body_draft: "after the unknown body".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice send ok");

    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    let mut after_seq = 0i64;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("an unknown body is a skipped record, never a failed walk");
    assert_eq!(outcome.ingested, 1, "only the ordinary message is ingested");
    assert_eq!(
        after_seq, 2,
        "the walk moved past both records — nothing stalls"
    );
    let bodies: Vec<String> = bob_manager
        .thread_detail(bob_thread_id)
        .expect("thread")
        .messages
        .iter()
        .map(|m| m.body.clone())
        .collect();
    assert!(
        bodies.iter().any(|b| b == "after the unknown body"),
        "the message after the unknown body still lands; got {bodies:?}"
    );
    assert!(
        !bodies.iter().any(|b| b.contains("Hologram")),
        "the unknown body never becomes a bubble; got {bodies:?}"
    );
}

// ── Legal takedown: a withheld record surfaces the shared tombstone ─────

/// The conversation half of the legal-obligation social-takedown carve-out
/// (`moderation.md` § Categories & enforcement item 1): when the nest has taken a
/// message down under a legal obligation it serves the record with the sealed
/// `envelope` **withheld** (empty) and carries the reference on
/// `ChannelFetchEntry.legal_takedown`. The shared inbound driver must NOT try to
/// decrypt the empty envelope — it ingests a tombstone `MessageSnapshot` carrying
/// `legal_takedown_ref = Some(reference)` and an empty body, so every app
/// paints `legalTakedownTombstone(reference)` in place of the bubble (never a
/// blank / failed-to-decrypt bubble). The tombstone keeps the message's `seq`
/// slot via its `conv:<channel>:<seq>` id.
#[tokio::test]
async fn inbound_driver_surfaces_a_legal_takedown_tombstone_for_a_withheld_record() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel);

    // Alice sends a message; it lands at seq 1 on the (mock) nest.
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend.bind_channel(ThreadId("a-1".into()), channel_id);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
            &ComposeState {
                body_draft: "unlawful content".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice send ok");

    // The nest takes that record (seq 1) down under a legal obligation: subsequent
    // fetches withhold its sealed envelope and carry the reference.
    let reference = "EU-DSA-2024/12345";
    nest.take_down(&channel_id.to_string(), 1, reference);

    // Bob's side: a manager + backend + a thread bound to the channel.
    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    // Drive the inbound feed: the withheld record is ingested as a tombstone, NOT
    // decrypted (its envelope is empty, so a decrypt attempt would have skipped it).
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(
        ingested, 1,
        "the withheld record ingests exactly one tombstone"
    );
    assert!(after_seq >= 1, "cursor advanced past the withheld record");

    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    let tombstone = detail
        .messages
        .iter()
        .find(|m| m.message_id.0 == format!("conv:{}:1", channel_id))
        .expect("the taken-down record is present in-thread at its seq slot");
    assert_eq!(
        tombstone.legal_takedown_ref.as_deref(),
        Some(reference),
        "the tombstone carries the legal reference the client renders"
    );
    assert!(
        tombstone.body.is_empty(),
        "the withheld body is empty — the client paints legalTakedownTombstone, never the content"
    );
    assert!(
        !tombstone.is_own,
        "a taken-down record is never attributed as the local actor's own"
    );
    // The real plaintext ("unlawful content") never reaches the thread — the nest
    // withheld it, so no message body carries it.
    assert!(
        detail.messages.iter().all(|m| !m.body.contains("unlawful")),
        "the withheld plaintext must never surface"
    );
}

// ── Moderation: the post-decrypt classify hook retains a local spam detection ─

/// The encrypted-mode social-content moderation signal: a fauna-native (MLS-sealed)
/// conversation message the nest cannot classify is classified **post-decrypt on the
/// receiver's client** and retained in the session-owned `LocalDetectionStore`
/// (`docs/goal/behavior/moderation.md` § Layout & flow; `content-scoring.md` § the two
/// plaintext positions → the client). Alice sends a spammy message; after Bob's inbound
/// driver decrypts + ingests it, Bob's store carries one spam detection — the queue's
/// local half. The classify hook lives in `ConversationsManager::ingest_inbound_to_thread`
/// (fed by `poll_inbound_conv`), so this drives the real decrypt→ingest→classify path.
#[tokio::test]
async fn inbound_driver_retains_post_decrypt_spam_local_detection() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend.bind_channel(ThreadId("a-1".into()), channel_id);
    // A body the shared `classify_text` flags as spam well above the 0.3 gate
    // (multiple spam phrases → confidence ≫ 300 per-mille).
    let spam_body = "BUY NOW!!! CLICK HERE for FREE MONEY!!! Limited time — act now!!!";
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
            &ComposeState {
                body_draft: spam_body.into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice send ok");

    // Bob's side: a manager with the FaunaMls backend + an installed local-detection
    // store (in production `ConversationsSession::from_manager` installs it).
    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    let store = Arc::new(Mutex::new(LocalDetectionStore::new()));
    bob_manager.set_local_detection_store(Arc::clone(&store));

    // Seed + bind Bob's thread (the setup ingest uses `ingest_inbound`, NOT the MLS
    // driver, so it never reaches the classify hook — the store stays empty until the
    // real decrypt).
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    assert!(
        store.lock().unwrap().is_empty(),
        "the participant-keyed setup ingest bypasses the MLS classify hook"
    );
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    // Drive the inbound feed: fetch → decode → MLS-decrypt → ingest → classify.
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(ingested, 1, "one inbound message ingested");

    // The post-decrypt classify hook retained exactly one spam local detection.
    let snap = store.lock().unwrap().snapshot();
    assert_eq!(
        snap.len(),
        1,
        "one spam local detection retained; got {snap:?}"
    );
    assert_eq!(snap[0].category, "spam", "spam-only local signal");
    assert!(
        snap[0].confidence_per_mille > 300,
        "confidence above the per-mille gate; got {}",
        snap[0].confidence_per_mille
    );
    assert_eq!(snap[0].content_type, "message");
}

// ── Track C: welcome ingest materializes + binds the receiver's thread ─

/// **A Welcome's members are seated through the shared resolution path**, so a
/// member this device already knows arrives by name rather than nameless.
///
/// `ingest_welcome` builds its participants from the joined group's engine
/// roster, which carries actor ids and nothing else — a leaf credential has no
/// handle to give. It seats through
/// `ConversationsManager::seat_address_for`, the one path it shares with
/// `apply_inbound_roster` (`conversation-rooms.md` § Implementation status
/// today: "one follow-on serving both this arm and the Welcome's").
///
/// Driven over a real `MlsEngine` and the production `ingest_welcome`, not a
/// hand-built roster: the property under test is that the *production* seat
/// consults what the device knows, and a hand-built participant list would
/// prove nothing about it.
#[tokio::test]
async fn a_welcomes_member_is_seated_by_a_handle_this_device_already_knows() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    // Bob has met Alice before, in some other thread, and renders her by name:
    // a thread HE started with her address, whose group seats her own key —
    // the only kind of seat that lends its handle (`contacts.md` § The private
    // overlay → *The paint gate*: a nameless seat is named only from a thread
    // whose bound group has proven that actor id).
    nest.seed_keypackage(
        &hex::encode(alice_actor.0),
        alice.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let bobs_own = bob_manager.create_mls_group(vec![fauna_addr("alice@nest", alice_actor)]);
    bob_manager.select_thread(bobs_own.clone());
    bob_manager.set_compose_body(bobs_own.clone(), "hi alice".into());
    bob_manager
        .send(bobs_own)
        .await
        .expect("bob bootstraps his own thread with alice");

    // Alice's Welcome came first: the one Bob is about to ingest.
    let welcome = nest.welcomes()[0].clone();
    assert_eq!(welcome.recipient_hex, hex::encode(bob_actor.0));
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");

    let detail = bob_manager.thread_detail(thread_id).expect("thread");
    let alice_seat = detail
        .participants
        .iter()
        .position(|p| p.person_actor_id() == Some(alice_actor))
        .expect("Alice is a participant of the materialized thread");
    assert_eq!(
        detail.participants[alice_seat].person_handle(),
        Some("alice@nest"),
        "the Welcome carries no handle, but this device already knew Alice's"
    );
    assert_eq!(
        detail.participant_displays[alice_seat], "alice@nest",
        "and the display column the user reads follows the seat"
    );
}

/// The honest half of the same seat: a Welcome from someone this device has
/// never met still renders a member — as the elided actor id
/// (`value-formatting.md` § Account display label), never as a blank row.
#[tokio::test]
async fn a_welcomes_unknown_member_renders_as_an_elided_id_not_a_blank_row() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    let welcome = nest.welcomes()[0].clone();
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");

    let detail = bob_manager.thread_detail(thread_id).expect("thread");
    let alice_seat = detail
        .participants
        .iter()
        .position(|p| p.person_actor_id() == Some(alice_actor))
        .expect("Alice is a participant of the materialized thread");
    assert_eq!(
        detail.participants[alice_seat].person_handle(),
        None,
        "nothing on this device knows Alice's handle, and the seat does not invent one"
    );
    assert_eq!(
        detail.participant_displays[alice_seat],
        fauna_core::format::short_id(&alice_actor.to_hex()),
        "the row still names somebody — a blank row is the defect this closes"
    );
}

#[tokio::test]
async fn welcome_then_inbound_end_to_end() {
    // Alice bootstraps a 1:1 with Bob and sends "ping" — the full Track-B send
    // path. The MockNest captures both the Welcome and the ciphertext.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    // Bob's side: feed the delivered Welcome to `ingest_welcome`. No manual
    // thread creation, no test-helpers — this is the production receive path.
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    let welcome = nest.welcomes()[0].clone();
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");

    // The materialized thread is FaunaMls, carries Alice as a participant, and
    // is bound to the channel (so the inbound driver can route into it).
    let detail = bob_manager
        .thread_detail(thread_id.clone())
        .expect("thread");
    assert_eq!(detail.rail, Rail::FaunaMls);
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "Alice is a participant of the materialized thread"
    );

    // Re-ingesting the same Welcome is idempotent (same thread, no duplicate).
    let again_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("idempotent welcome");
    assert_eq!(
        again_id, thread_id,
        "re-delivered welcome reuses the thread"
    );
    assert_eq!(
        bob_manager.snapshot().threads.len(),
        1,
        "no duplicate thread on re-delivery"
    );

    // Now the inbound driver routes Alice's "ping" into the materialized thread.
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(ingested, 1);
    let detail = bob_manager.thread_detail(thread_id).expect("thread");
    assert!(detail.messages.iter().any(|m| m.body == "ping"));
}

/// Parse a channel hex string back into a `ChannelId` for the driver call.
fn channel_id_from_hex(hex_str: &str) -> fauna_mls::types::ChannelId {
    fauna_mls::types::ChannelId::from_hex(hex_str).expect("valid channel hex")
}

// ── Attachments: seal + upload outbound, fetch + open + cache inbound ───
//
// The FaunaMls twin of the SMTP rail's `attachment_round_trips_outbound_to_inbound`
// (`smtp_backend_tests.rs`): instead of inlining the bytes in a MIME part, the
// MLS rail seals each attachment under the channel's `derive_blob_key(epoch_secret)`,
// uploads the sealed blob to the (mock) nest content-addressed store, and
// references it by `sealed_cid` + the uniform `blob_hash` in the channel message.
// The receiver GETs the blob, opens it under the message's epoch key, caches the
// plaintext, and renders the `AttachmentSnapshot` — same content handle both ways
// (`docs/goal/ui/conversations.md` § Attachments + § Encryption at rest).

#[tokio::test]
async fn attachment_round_trips_outbound_to_inbound_faunamls() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\nnot-a-real-image-but-distinct-bytes";
    // The uniform content handle is the BLAKE3 of the *plaintext* (what
    // `manager.add_attachment` computes), independent of the seal.
    let blob_hash = hex::encode(blake3::hash(png).as_bytes());

    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    // Alice bootstraps a 1:1 with Bob and sends a message carrying an attachment.
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let compose = ComposeState {
        body_draft: "see attached".into(),
        ..Default::default()
    };
    let attachment = ResolvedAttachment {
        blob_hash: blob_hash.clone(),
        filename: "pic.png".into(),
        mime_type: "image/png".into(),
        is_image: true,
        bytes: png.to_vec(),
    };
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &compose,
            std::slice::from_ref(&attachment),
        )
        .await
        .expect("alice bootstrap + send with attachment");

    // Exactly one channel message (the attachment bubble) and one sealed blob —
    // the blob bytes on the nest are NOT the plaintext (sealed under the channel
    // key), and the store key is their BLAKE3 (a content address ≠ the plaintext
    // handle).
    assert_eq!(nest.total_sends(), 1, "one channel message posted");
    {
        // Scoped so the guard drops before the receive-side awaits below.
        let blobs = nest.blobs.lock().unwrap();
        assert_eq!(blobs.len(), 1, "one sealed attachment blob uploaded");
        let (sealed_cid_hex, sealed) = blobs.iter().next().unwrap();
        assert_ne!(
            sealed.as_slice(),
            png,
            "stored blob is sealed, not plaintext"
        );
        assert_ne!(
            sealed_cid_hex, &blob_hash,
            "store key is the sealed cid, not the plaintext handle"
        );
    }

    // Bob ingests the welcome, then polls: the driver fetches + opens + caches the
    // blob and yields a bubble carrying the rendered AttachmentSnapshot.
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    let welcome = nest.welcomes()[0].clone();

    // The conversation kind's blob-reachability floor: the send listed the
    // sealed blob's content address in plaintext beside the envelope, and it
    // is exactly the key the blob was uploaded under — the nest cannot open
    // the body, so this list is what keeps the blob alive past the GC's grace
    // (`encryption-at-rest.md` § Per-content-kind conformance → Conversation
    // messages row, 2026-09-08).
    {
        let sealed_cids: Vec<String> = nest.blobs.lock().unwrap().keys().cloned().collect();
        assert_eq!(
            nest.sent_attachment_refs(&welcome.channel_hex),
            vec![sealed_cids],
            "the attachment send names its sealed cid in plaintext attachment_refs"
        );
    }

    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");

    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(ingested, 1, "one inbound attachment message ingested");

    let detail = bob_manager.thread_detail(thread_id).expect("thread");
    let msg = detail
        .messages
        .iter()
        .find(|m| !attachment_blocks(&m.document).is_empty())
        .expect("an attachment-bearing bubble");
    assert_eq!(msg.body, "see attached", "caption preserved");
    let atts = attachment_blocks(&msg.document);
    assert_eq!(atts.len(), 1);
    let att = &atts[0];
    assert_eq!(att.filename, "pic.png");
    assert_eq!(att.mime_type, "image/png");
    assert!(att.is_image);
    assert_eq!(att.size_bytes, png.len() as u64);
    assert!(
        !att.c2pa,
        "synthetic non-image bytes must not be flagged C2PA-bearing"
    );
    // The content handle agrees with the sender's plaintext hash, and the bytes
    // round-trip: sealed → uploaded → fetched → opened → cached under the handle.
    assert_eq!(att.blob_hash, blob_hash, "content handle agrees both ways");
    assert_eq!(
        bob_manager
            .attachment_bytes(att.blob_hash.clone())
            .as_deref(),
        Some(png),
        "inbound bytes cached + loadable via the handle"
    );
}

/// **A room's attachment bytes rest on the room's home nest**
/// (`conversation-rooms.md` § The home nest → *Attachment bytes*, ratified
/// 2026-09-09): the seam's `blob_put` / `blob_get` carry the channel's recorded
/// home — `None` for a same-nest channel, `Some(url)` for a foreign-homed one —
/// by the same `ChannelHome` signal that picks `send` vs `send_remote`, so the
/// upload lands beside the record that pins it and the read asks the nest that
/// holds it. Discriminating by construction: before this the seam had no home
/// parameter at all, and the impl read the member's own nest either way.
#[tokio::test]
async fn attachment_blob_put_and_get_route_to_the_channels_home_nest() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\nrouted-by-home";
    let blob_hash = hex::encode(blake3::hash(png).as_bytes());
    let attachment = ResolvedAttachment {
        blob_hash: blob_hash.clone(),
        filename: "pic.png".into(),
        mime_type: "image/png".into(),
        is_image: true,
        bytes: png.to_vec(),
    };

    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    // Same-nest: the bootstrap send's upload carries no home (`None`).
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread = fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]);
    let compose = ComposeState {
        body_draft: "see attached".into(),
        ..Default::default()
    };
    alice_backend
        .send(&thread, &compose, std::slice::from_ref(&attachment))
        .await
        .expect("bootstrap + same-nest attachment send");
    assert_eq!(
        nest.blob_homes.lock().unwrap().as_slice(),
        &[None],
        "a same-nest channel's upload carries no home url"
    );

    // Foreign-homed: once the channel's home is recorded (as a Welcome ingest
    // does), the next upload is routed to that home.
    let channel = alice_backend.bound_channels()[0];
    // (The upload precedes the send — `encode_body` seals + uploads before
    // `send_on_channel` picks `send_remote` — and this mock has no federation
    // relay, so the send itself is refused; the routing under test has already
    // happened by then.)
    alice_backend.record_channel_home(channel, "https://home.example");
    let refused = alice_backend
        .send(&thread, &compose, std::slice::from_ref(&attachment))
        .await
        .expect_err("the mock nest refuses the relayed send (no federation relay)");
    assert!(
        refused.to_string().contains("send_remote"),
        "the send was routed to send_remote: {refused}"
    );
    assert_eq!(
        nest.blob_homes
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("https://home.example"),
        "a foreign-homed channel's upload is routed to the channel's HOME nest"
    );

    // And the receive side reads from the same home: Bob ingests the Welcome
    // with the home url and his attachment fetch carries it.
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    let welcome = nest.welcomes()[0].clone();
    ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "https://home.example",
    )
    .await
    .expect("welcome ingest");
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(ingested, 1, "the same-nest attachment message ingested");
    let homes = nest.blob_homes.lock().unwrap();
    let gets = &homes[2..];
    assert_eq!(gets.len(), 1, "one blob_get for the attachment message");
    assert!(
        gets.iter()
            .all(|h| h.as_deref() == Some("https://home.example")),
        "every foreign-homed read asks the channel's HOME nest: {gets:?}"
    );
}

/// Receive-time C2PA detection is real on native builds — `attachments_to_inbound`
/// probes the decrypted bytes via `fauna_media::process::detect_c2pa`
/// (`docs/goal/ui/conversations.md` § Attachments "C2PA on-device"). A genuine
/// C2PA-signed image, round-tripped through the same seal → upload → fetch →
/// open path as the test above, must land with `c2pa == true` on the
/// receiver — not the hard-coded `false` the render used to carry regardless
/// of what the bytes actually held.
#[tokio::test]
async fn attachment_c2pa_detected_on_inbound_for_signed_image() {
    let signed_png: &[u8] = include_bytes!("../../../tests/fixtures/c2pa-signed.png");
    let blob_hash = hex::encode(blake3::hash(signed_png).as_bytes());

    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let compose = ComposeState {
        body_draft: "see attached".into(),
        ..Default::default()
    };
    let attachment = ResolvedAttachment {
        blob_hash: blob_hash.clone(),
        filename: "signed.png".into(),
        mime_type: "image/png".into(),
        is_image: true,
        bytes: signed_png.to_vec(),
    };
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-2".into()), vec![fauna_addr("bob", bob_actor)]),
            &compose,
            std::slice::from_ref(&attachment),
        )
        .await
        .expect("alice bootstrap + send with attachment");

    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    let welcome = nest.welcomes()[0].clone();
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");

    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(ingested, 1, "one inbound attachment message ingested");

    let detail = bob_manager.thread_detail(thread_id).expect("thread");
    let msg = detail
        .messages
        .iter()
        .find(|m| !attachment_blocks(&m.document).is_empty())
        .expect("an attachment-bearing bubble");
    let atts = attachment_blocks(&msg.document);
    assert_eq!(atts.len(), 1);
    assert!(
        atts[0].c2pa,
        "a real C2PA-signed image must be detected on receive, not hard-coded false"
    );
}

// ── Track D: membership (add / remove / rename) ────────────────────────

/// Adding a member to an existing (already-bound) MLS group adds **in place**:
/// the backend fetches the new member's key package, posts an MLS `Commit` so
/// every existing member ratchets forward, and delivers a Welcome to the new
/// member. After the add, an application message Alice sends decrypts for both
/// the pre-existing member (who processed the commit) and the new member (who
/// joined from the Welcome).
#[tokio::test]
async fn add_participant_commits_and_welcomes_new_group_member() {
    // Alice forms a 2-person group with Bob (the create_group path).
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    // Carol publishes a key package to the nest, ready to be added.
    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    nest.seed_keypackage(
        &carol_hex,
        carol.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("add ok");

    // A Welcome was delivered to Carol, flagged as a group welcome with the
    // raw group id (so the receiver materializes a group thread, not a DM).
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "one welcome to the new member");
    assert_eq!(welcomes[0].recipient_hex, carol_hex);
    match &welcomes[0].kind {
        WelcomeChannelKind::Group { group_id_hex } => {
            assert!(
                !group_id_hex.is_empty(),
                "group welcome carries the group id"
            );
        }
        other => panic!("in-place group add is a group welcome, got {other:?}"),
    }

    // A single Commit envelope was posted to the channel (not an Application).
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one commit posted");
    let commit_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("add must post a Commit envelope"),
    };

    // Bob ratchets forward by processing the commit; Carol joins from the
    // Welcome. Both are now in the post-add epoch.
    bob.process_commit(&channel_id, &commit_bytes)
        .expect("bob processes add commit");
    let carol_channel = carol
        .join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .expect("carol joins from welcome");
    assert_eq!(carol_channel, channel_id);

    // Alice now sends to the 3-person group; both Bob and Carol decrypt it.
    backend
        .send(
            &fauna_mls_thread(thread_id, vec![]),
            &ComposeState {
                body_draft: "hi all".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("post-add send ok");
    let app_env = nest.sent_envelopes(&channel_id.to_string());
    let app_bytes = match ChannelEnvelope::from_bytes(app_env.last().unwrap()).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    let bob_msg = bob.decrypt(&channel_id, &app_bytes).expect("bob decrypts");
    let carol_msg = carol
        .decrypt(&channel_id, &app_bytes)
        .expect("carol decrypts");
    assert!(matches!(bob_msg.body, ChannelMessageBody::Text(t) if t == "hi all"));
    assert!(matches!(carol_msg.body, ChannelMessageBody::Text(t) if t == "hi all"));
}

/// **R4 (account-data-plane.md § The ratified decisions) — chat add-participant Welcome-loss crash window.** Unlike the
/// send-failure test above (nothing merges — a plain retryable error), a fault
/// AFTER the send succeeds — modeling a crash/network fault between
/// `merge_pending_commit` and the `welcome_deliver` RPC returning — leaves the
/// group epoch ADVANCED: carol is a genuine leaf every other member's next
/// `process_commit` will admit, but her Welcome, which lived only in
/// `add_participant`'s local variable, never reached the nest and cannot be
/// re-minted.
///
/// The heal is therefore **evict-then-re-admit**, not re-delivery: a Welcome
/// exists only inside the Add commit that mints it, so the only way to produce
/// one for carol is to admit her again — which first requires her ghost leaf
/// gone. The discriminator is the nest's routing roster, written at Welcome
/// delivery (`mls-group-key-material.md` § M2 *Admitting a member*), because
/// local MLS state cannot tell a phantom leaf from a healthy member.
///
/// Was RED, pinned `#[ignore]`d, until the heal
/// landed; the pre-fix retry surfaced
/// `Transport("OpenMLS error: CreateCommitError(ProposalValidationError(DuplicateSignatureKey))")`.
#[tokio::test]
async fn add_participant_crash_after_merge_before_welcome_deliver_leaves_a_phantom_leaf() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    // Two key packages: the crash-window attempt consumes one fetch, the retry the next.
    for kp in carol.generate_key_packages_bytes(2).unwrap() {
        nest.seed_keypackage(&carol_hex, kp);
    }

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);
    let epoch_before = alice.current_epoch(&channel_id).unwrap();

    // Simulate the crash window: the add commit sends and merges (unlike the
    // send-failure test), but welcome_deliver never lands.
    nest.fail_welcome_deliver(true);
    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect_err("a lost welcome_deliver must surface as an error");

    // The crash window itself: the commit already merged, and carol is a
    // phantom leaf — admitted with no way to join.
    assert!(
        alice.current_epoch(&channel_id).unwrap() > epoch_before,
        "the add commit merged before the failed welcome_deliver (the crash window)"
    );
    assert!(
        alice
            .find_leaf_by_identity(&channel_id, &carol_actor)
            .is_some(),
        "carol is a phantom leaf: admitted into the group with no way to join"
    );
    assert!(
        nest.welcomes().is_empty(),
        "the crash window is exactly a Welcome that never landed"
    );

    // Every other real member eventually ratchets onto the same commit — the
    // epoch advance is not locally scoped to alice.
    let commit_bytes = match ChannelEnvelope::from_bytes(
        nest.sent_envelopes(&channel_id.to_string()).last().unwrap(),
    )
    .unwrap()
    {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("add must post a Commit envelope"),
    };
    bob.process_commit(&channel_id, &commit_bytes)
        .expect("bob ratchets onto the phantom leaf's epoch too");

    // The roster is what makes the phantom visible: carol is an MLS leaf, but
    // the failed delivery never registered her, so she reads as not-yet-invited.
    assert_eq!(
        nest.channel_actors(channel_id.to_string(), None)
            .await
            .unwrap(),
        Some(vec![]),
        "the phantom leaf is off the roster — the discriminator the heal reads"
    );

    // R4's heal: the retry sees a seated-but-unrostered identity, evicts the
    // ghost leaf, and admits carol fresh — which is the only way to mint her a
    // Welcome at all.
    nest.fail_welcome_deliver(false);
    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("the retry heals the phantom leaf");

    // Carol is seated once, on the roster, and — the point of the whole
    // exercise — holds a Welcome she can actually join with.
    assert!(
        alice
            .find_leaf_by_identity(&channel_id, &carol_actor)
            .is_some(),
        "carol is a member after the heal"
    );
    assert_eq!(
        nest.channel_actors(channel_id.to_string(), None)
            .await
            .unwrap(),
        Some(vec![carol_hex.clone()]),
        "the heal's delivery registered her"
    );
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "exactly one Welcome landed (the heal's)");
    let carol_channel = carol
        .join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .expect("carol joins from the heal's Welcome — the crash window is closed");
    assert_eq!(carol_channel, channel_id);

    // Bob follows the heal's two commits (evict + re-admit) and lands in the
    // same epoch as carol: alice's next message decrypts for both.
    for env in nest.sent_envelopes(&channel_id.to_string()).iter().skip(1) {
        if let ChannelEnvelope::Commit(b) = ChannelEnvelope::from_bytes(env).unwrap() {
            bob.process_commit(&channel_id, &b)
                .expect("bob ratchets through the heal");
        }
    }
    backend
        .send(
            &fauna_mls_thread(thread_id, vec![]),
            &ComposeState {
                body_draft: "welcome carol".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("post-heal send ok");
    let app_env = nest.sent_envelopes(&channel_id.to_string());
    let app_bytes = match ChannelEnvelope::from_bytes(app_env.last().unwrap()).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    for (who, engine) in [("bob", &bob), ("carol", &carol)] {
        let msg = engine
            .decrypt(&channel_id, &app_bytes)
            .unwrap_or_else(|e| panic!("{who} decrypts after the heal: {e}"));
        assert!(matches!(msg.body, ChannelMessageBody::Text(t) if t == "welcome carol"));
    }
}

/// The heal's discriminator must not fire on a **healthy** member: adding
/// someone already in the group (a stale overlay, a second device, a retry
/// whose first attempt actually landed — the manager does not filter these,
/// `manager.rs::add_participant_inner`) is an idempotent no-op, NOT an
/// evict-and-re-invite. Local MLS state alone cannot tell this case from the
/// phantom above; the roster row is the whole difference.
#[tokio::test]
async fn add_participant_is_a_no_op_for_a_member_already_on_the_roster() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    for kp in carol.generate_key_packages_bytes(2).unwrap() {
        nest.seed_keypackage(&carol_hex, kp);
    }
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("first add ok");
    let epoch_after_add = alice.current_epoch(&channel_id).unwrap();
    let sends_after_add = nest.sent_envelopes(&channel_id.to_string()).len();

    // The duplicate gesture.
    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("adding a healthy member again is a no-op, not an error");

    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_after_add,
        "no commit: a healthy member is never cut off and re-invited"
    );
    assert_eq!(
        nest.sent_envelopes(&channel_id.to_string()).len(),
        sends_after_add,
        "no envelope posted"
    );
    assert_eq!(nest.welcomes().len(), 1, "no second Welcome");
    assert_eq!(
        nest.keypackage_count_for(&carol_hex),
        1,
        "the no-op does not burn a key package"
    );
}

/// With the roster unreadable — a transport failure reading
/// `fauna.conversations.channel.actors`, or a foreign-homed channel whose
/// authoritative roster lives on its home nest — the add must **refuse to
/// guess**. Evicting a member we cannot vouch for is the one outcome strictly
/// worse than doing nothing, so this surfaces a specific, actionable error
/// rather than either healing blindly or leaking MLS's raw
/// duplicate-credential string.
#[tokio::test]
async fn add_participant_refuses_to_guess_when_the_roster_is_unreadable() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    for kp in carol.generate_key_packages_bytes(2).unwrap() {
        nest.seed_keypackage(&carol_hex, kp);
    }
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("first add ok");
    let epoch_after_add = alice.current_epoch(&channel_id).unwrap();

    nest.make_roster_unreadable(true);
    let err = backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect_err("an unverifiable duplicate add must not silently proceed");

    let msg = err.to_string();
    assert!(
        msg.contains(&carol_hex) && msg.contains("remove them from the group"),
        "the error names who and what to do, actionably: {msg}"
    );
    assert!(
        !msg.contains("DuplicateSignatureKey"),
        "never MLS's raw duplicate-credential string: {msg}"
    );
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_after_add,
        "refusing to guess means touching nothing"
    );
}

/// On a **foreign-homed** channel (this member joined from a cross-nest
/// Welcome; the channel's log — and its authoritative roster — live on the
/// home nest), a duplicate add of a healthy member is the same idempotent
/// no-op as on the home nest: `roster_holds` passes the recorded home URL
/// through the seam, which rides the **distinct relay kind**
/// `channel.actors_remote` → `fauna.federation.channel.actors` to the home's
/// authoritative union (`federation.md` § Cross-nest). **Inverted from the
/// slice-1 pin** that expected the refuse arm here — slice 2 upgraded the
/// refuse to the relay, and an added-only test would have left two green
/// tests asserting opposites. The skew sibling below keeps the refuse arm
/// pinned for the nest that does not know the kind.
#[tokio::test]
async fn add_participant_on_a_foreign_homed_channel_heals_via_the_home_roster_relay() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    for kp in carol.generate_key_packages_bytes(2).unwrap() {
        nest.seed_keypackage(&carol_hex, kp);
    }
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("first add ok");
    let epoch_after_add = alice.current_epoch(&channel_id).unwrap();

    // The channel is foreign-homed: this backend learned it from a cross-nest
    // Welcome envelope's `nest_url` (the same signal that relays fetch/send).
    backend.record_channel_home(channel_id, "https://home.example");

    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("a foreign-homed duplicate add of a healthy member is a no-op via the relay");
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_after_add,
        "no-op means no commit: the epoch is untouched (an evict + re-admit advances it twice)"
    );
    assert_eq!(
        nest.welcomes().len(),
        1,
        "no second Welcome was minted for the healthy member"
    );
    assert_eq!(
        nest.actors_read_urls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
            .as_deref(),
        Some("https://home.example"),
        "the roster read must carry the channel's home URL through the seam \
         (the relay-kind pick), never take the same-nest path"
    );
}

/// Version skew: the same foreign-homed duplicate add against a nest that does
/// NOT know `channel.actors_remote` lands in the REFUSE arm — never
/// partial-roster trust, never an evict. `OutdatedNest` leaves
/// `channel_actors` at the seam's default `Ok(None)` ("roster unreadable"),
/// exactly what the real seam degrades to when the kind is unknown on either
/// side of the relay — the reason the client leg is a **distinct kind**, not
/// an additive field a stale nest would answer with a clean partial roster
/// (`federation.md` § Cross-nest, the fetch-vs-actors dividing line).
#[tokio::test]
async fn add_participant_on_a_foreign_homed_channel_refuses_under_version_skew() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), Arc::new(OutdatedNest), "alice", alice_actor);
    let thread_id = ThreadId("g-skew".into());
    backend.bind_channel(thread_id.clone(), channel_id);
    backend.record_channel_home(channel_id, "https://home.example");

    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);
    let epoch_before = alice.current_epoch(&channel_id).unwrap();
    let err = backend
        .add_participant(thread_id, fauna_addr("bob", bob_actor))
        .await
        .expect_err("an unreadable roster refuses; it never evicts on a guess");
    let msg = err.to_string();
    assert!(
        msg.contains(&bob_hex) && msg.contains("remove them from the group"),
        "the error names who and what to do, actionably: {msg}"
    );
    assert!(
        !msg.contains("DuplicateSignatureKey"),
        "never MLS's raw duplicate-credential string: {msg}"
    );
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "refusing to guess means touching nothing"
    );
}

/// Adding **yourself** must never route into the heal. The owner's own leaf
/// answers `find_leaf_by_identity` exactly like a newcomer's, and an owner who
/// has not yet posted is not on the roster either — so an unguarded heal would
/// read "phantom" and evict the caller from their own group.
#[tokio::test]
async fn add_participant_refuses_the_local_identity_and_never_evicts_it() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);
    let epoch_before = alice.current_epoch(&channel_id).unwrap();

    // Alice is deliberately NOT on the roster here (she has posted nothing), so
    // the roster alone would classify her as a phantom.
    let err = backend
        .add_participant(thread_id.clone(), fauna_addr("alice", alice_actor))
        .await
        .expect_err("adding yourself is refused");
    assert!(err.to_string().contains("already a member"), "{err}");

    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "no commit — above all, alice did not evict her own leaf"
    );
    assert!(
        alice
            .find_leaf_by_identity(&channel_id, &alice_actor)
            .is_some(),
        "alice is still seated in her own group"
    );
}

/// Removing a participant posts an MLS `Commit` (no Welcome): the remaining
/// members ratchet to a new epoch the removed member can't follow. After the
/// remove, an application message decrypts for the member who stayed but fails
/// for the one who was removed.
#[tokio::test]
async fn remove_participant_commits_and_cuts_off_removed_member() {
    // Alice forms a 3-person group with Bob and Carol in one create_group.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
    carol.join_from_welcome_bytes(&welcome_bytes).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .remove_participant(thread_id.clone(), fauna_addr("bob", bob_actor))
        .await
        .expect("remove ok");

    // A Commit was posted; no Welcome on a removal.
    assert!(nest.welcomes().is_empty(), "removal delivers no welcome");
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one commit posted");
    let commit_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("remove must post a Commit envelope"),
    };

    // Carol stays and processes the commit; Bob (removed) cannot apply it.
    carol
        .process_commit(&channel_id, &commit_bytes)
        .expect("carol processes remove commit");

    // Alice sends to the now-2-person group: Carol decrypts, Bob is cut off.
    backend
        .send(
            &fauna_mls_thread(thread_id, vec![]),
            &ComposeState {
                body_draft: "just us".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("post-remove send ok");
    let app_bytes = match ChannelEnvelope::from_bytes(
        nest.sent_envelopes(&channel_id.to_string()).last().unwrap(),
    )
    .unwrap()
    {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    let carol_msg = carol
        .decrypt(&channel_id, &app_bytes)
        .expect("carol decrypts");
    assert!(matches!(carol_msg.body, ChannelMessageBody::Text(t) if t == "just us"));
    assert!(
        bob.decrypt(&channel_id, &app_bytes).is_err(),
        "removed member can no longer decrypt"
    );
}

/// **The production wiring of the eviction driver's roster source**.
/// The three manager-level pins for that driver run
/// over `MockRailBackend`, which can only prove the *driver* asks the rail; a
/// mock agreeing with a broken implementation proves nothing about the rail's
/// answer. This asserts the answer against a real engine, and it asserts it by
/// **equality with `MlsEngine::group_members`** rather than against a
/// hand-listed roster — that call is literally the one the flag is raised off
/// (`fauna_client_recovery::group_sweep::unattested_members`), so the "both must
/// read the same source" contract is what the assertion is made of, and a future
/// edit that swapped in some other roster read could not keep it green.
#[tokio::test]
async fn the_authoritative_roster_is_the_engine_roster_the_flag_is_raised_off() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
    carol.join_from_welcome_bytes(&welcome_bytes).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    let roster = backend
        .authoritative_roster(&thread_id)
        .expect("a bound thread's rail is the authority for it");
    assert_eq!(
        roster,
        alice.group_members(&channel_id),
        "the rail answers with the engine roster itself"
    );
    assert!(
        roster.contains(&bob_actor),
        "and that roster really seats the group's members: {roster:?}"
    );

    // It tracks the engine rather than snapshotting it: the eviction commit
    // below is the only thing that changes, and the answer follows.
    backend
        .remove_participant(thread_id.clone(), fauna_addr("bob", bob_actor))
        .await
        .expect("remove ok");
    let after = backend
        .authoritative_roster(&thread_id)
        .expect("still bound");
    assert!(
        !after.contains(&bob_actor),
        "an evicted member is gone from the authority, which is what makes a \
         retry target only the remainder: {after:?}"
    );

    assert_eq!(
        backend.authoritative_roster(&ThreadId("never-bootstrapped".into())),
        None,
        "an unbound thread has no MLS group to ask, so the rail claims no \
         authority for it and the thread store's participant list stands — a \
         group the owner composed but never bootstrapped must not read as \
         'authoritatively nobody' and be skipped"
    );
}

/// **The production wiring of rule (5)'s seat classification** (the
/// ratification, `identity-succession.md` § Propagation). The manager-level
/// class-dispatch pins run over `MockRailBackend`, which can only prove the
/// driver dispatches on what the rail reports — a mock agreeing with a broken
/// classifier proves nothing about the real one. This asserts the classes
/// against a real engine, and it asserts the folder class **by equality with
/// [`FaunaMlsBackend::folder_poll_channels`]** — the derivation the folder
/// sweep itself routes on — so "the eviction and the sweep classify from one
/// source" is what the assertion is made of.
///
/// The relaunch half is the reason the durable scheduling marker exists: the
/// in-memory scheduling set empties on relaunch while the engine persists its
/// groups, so before the marker a relaunched (or organizer-created, never
/// in-set) scheduling one-off classified as folder — harmless for the sweep,
/// verdict-blocking-with-no-remedy for the eviction.
#[tokio::test]
async fn unbound_seats_are_classified_from_the_engine_and_survive_a_relaunch() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    // Four engine groups, one per class, each seating bob.
    let mk = || {
        let kp = bob.generate_key_packages(1).unwrap();
        let (ch, welcome) = alice.create_group(&kp).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        ch
    };
    let bound_chat = mk();
    let unbound_chat = mk();
    let scheduling = mk();
    let folder = mk();
    backend.bind_channel(ThreadId("g-bound".into()), bound_chat);
    // A chat group whose binding did not survive the relaunch: the durable
    // marker is there (a past bind stamped it), the RAM binding is not.
    alice.mark_channel_chat(&unbound_chat);
    backend.mark_scheduling_channel(scheduling);
    // A group bob is NOT in must not be reported as his seat. (Unbound and
    // unmarked, it still classifies folder rail for the person-independent
    // poll below — the seat filter is what keeps it out of bob's list.)
    let kp = MlsEngine::new_in_memory(ActorKeypair::generate())
        .unwrap()
        .generate_key_packages(1)
        .unwrap();
    let (bobless, _) = alice.create_group(&kp).unwrap();

    let mut seats = backend.unbound_seats_of(&bob_actor);
    seats.sort_by(|a, b| a.channel_hex.cmp(&b.channel_hex));
    let mut expected = vec![
        UnboundSeat {
            channel_hex: unbound_chat.to_string(),
            class: UnboundChannelClass::Chat,
        },
        UnboundSeat {
            channel_hex: scheduling.to_string(),
            class: UnboundChannelClass::Scheduling,
        },
        UnboundSeat {
            channel_hex: folder.to_string(),
            class: UnboundChannelClass::Folder,
        },
    ];
    expected.sort_by(|a, b| a.channel_hex.cmp(&b.channel_hex));
    assert_eq!(
        seats, expected,
        "exactly the three unbound seats, each with its class — the bound \
         group is the thread loop's business and the bob-less group is nobody's"
    );

    // The folder class IS the sweep's derivation, by equality with it: the
    // poll is person-independent, so it holds exactly the Folder-classified
    // channel plus the bob-less control — and nothing of any other class.
    let mut poll: Vec<String> = backend
        .folder_poll_channels()
        .into_iter()
        .map(|c| c.to_string())
        .collect();
    poll.sort();
    let mut expected_poll = vec![folder.to_string(), bobless.to_string()];
    expected_poll.sort();
    assert_eq!(
        poll, expected_poll,
        "one source: what classifies Folder here is exactly what the folder \
         sweep polls"
    );

    // A relaunch: a fresh backend over the same persisted engine, in-memory
    // marker sets empty. The durable markers keep every class — before them
    // the scheduling one-off degraded to Folder, which for the eviction is a
    // blocked verdict with a remedy that does not exist.
    let relaunched = FaunaMlsBackend::new(alice.clone(), nest, "alice", alice_actor);
    let mut after = relaunched.unbound_seats_of(&bob_actor);
    after.sort_by(|a, b| a.channel_hex.cmp(&b.channel_hex));
    let mut expected_after = expected.clone();
    // The bound thread's RAM binding died with the "process": the durable chat
    // marker is what keeps that channel out of the folder class.
    expected_after.push(UnboundSeat {
        channel_hex: bound_chat.to_string(),
        class: UnboundChannelClass::Chat,
    });
    expected_after.sort_by(|a, b| a.channel_hex.cmp(&b.channel_hex));
    assert_eq!(
        after, expected_after,
        "every class survives the relaunch off the engine's durable markers"
    );
}

/// **The membership half of the account plane's content-scope set** —
/// [`FaunaMlsBackend::conv_channels`], which
/// `fauna_sync_engine::account_runtime` reads once per pump pass to derive this
/// replica's `conv` scopes (`account-sync-plane.md` § Feeds and cursors →
/// *Scope partition*).
///
/// The classes are the same four the sweep classifies, so this pins the split
/// from the other side: a chat channel counts whether it is bound in this
/// process or merely carries the durable marker, and no channel of another
/// class does — including the "unknown kind" ones, which the narrow side
/// deliberately leaves out rather than minting a `conv` scope for every file
/// set. Restart-durability is the same durable-marker story: a fresh backend
/// over the same engine still reports both chat channels.
#[tokio::test]
async fn conv_channels_are_the_chat_channels_bound_or_durably_marked() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    let mk = || {
        let kp = bob.generate_key_packages(1).unwrap();
        let (ch, welcome) = alice.create_group(&kp).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        ch
    };
    let bound_chat = mk();
    let unbound_chat = mk();
    let scheduling = mk();
    let folder = mk(); // unknown kind: neither bound, marked, nor scheduling
    backend.bind_channel(ThreadId("g-bound".into()), bound_chat);
    alice.mark_channel_chat(&unbound_chat);
    backend.mark_scheduling_channel(scheduling);

    let convs: Vec<String> = backend
        .conv_channels()
        .into_iter()
        .map(|c| c.to_string())
        .collect();
    let mut expected = vec![bound_chat.to_string(), unbound_chat.to_string()];
    expected.sort();
    assert_eq!(
        convs, expected,
        "exactly the chat channels — the scheduling one-off and the unknown-kind \
         group mint no content scope"
    );
    assert!(
        !convs.contains(&folder.to_string()),
        "an unknown-kind group is not a conversation scope"
    );

    // The RAM binding dies with the process; the durable marker is what carries
    // a bound thread across a relaunch — the same property the sweep relies on.
    let relaunched = FaunaMlsBackend::new(alice.clone(), nest, "alice", alice_actor);
    let after: Vec<String> = relaunched
        .conv_channels()
        .into_iter()
        .map(|c| c.to_string())
        .collect();
    assert_eq!(after, expected, "both chat channels survive the relaunch");
}

/// **Rule 1 (merge-ordering), ungated add** — `devices.md` § Cross-device MLS
/// group-state sync, Durability rules. A failed `channel_send` must leave the
/// group **unmerged**: with the old optimistic path (`add_member` merges, then
/// sends) a send failure left the author at an epoch whose commit no other
/// member could ever receive — MLS cannot re-issue a commit for an
/// already-merged transition, members cannot skip epochs, and the debounced
/// replica autosave durably captures the merged epoch off *any* channel's
/// activity — so every member was stranded permanently. The staged path
/// (stage → send → merge-on-accept / clear-on-failure) makes the failure a
/// plain retryable error: nothing merged, nothing delivered, and the retry
/// converges everyone.
#[tokio::test]
async fn add_participant_send_failure_merges_nothing_and_is_retryable() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_actor = carol.identity_actor_id();
    let carol_hex = hex::encode(carol_actor.0);
    // Two key packages: the failed attempt consumes one fetch, the retry the next.
    for kp in carol.generate_key_packages_bytes(2).unwrap() {
        nest.seed_keypackage(&carol_hex, kp);
    }

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);
    let epoch_before = alice.current_epoch(&channel_id).unwrap();

    // The commit send fails: the add must surface the error WITHOUT merging.
    nest.fail_channel_sends(true);
    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect_err("send fault must fail the add");
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "a failed send must not advance the epoch (Rule 1: merge only after the \
         commit bytes are durable on the log)"
    );
    assert!(
        nest.welcomes().is_empty(),
        "no Welcome for a commit that never landed"
    );
    assert!(
        nest.sent_envelopes(&channel_id.to_string()).is_empty(),
        "nothing reached the log"
    );

    // Transport recovers: the retry succeeds end-to-end from the intact state.
    nest.fail_channel_sends(false);
    backend
        .add_participant(thread_id.clone(), fauna_addr("carol", carol_actor))
        .await
        .expect("retry succeeds");
    assert!(
        alice.current_epoch(&channel_id).unwrap() > epoch_before,
        "the accepted retry merged"
    );
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "exactly the retry's commit on the log");
    let commit_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("add must post a Commit envelope"),
    };
    bob.process_commit(&channel_id, &commit_bytes)
        .expect("bob ratchets on the retry's commit");
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "one welcome, from the successful retry");
    carol
        .join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .expect("carol joins from the retry's welcome");
    assert_eq!(
        bob.current_epoch(&channel_id).unwrap(),
        alice.current_epoch(&channel_id).unwrap(),
        "all members converge on the retried add"
    );
}

/// **Rule 1 (merge-ordering), ungated remove** — the removal twin of the test
/// above. A failed send leaves the member in place at the old epoch (alice can
/// still talk to bob), and the retry cleanly re-stages, lands, and cuts the
/// removed member off.
#[tokio::test]
async fn remove_participant_send_failure_merges_nothing_and_is_retryable() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
    carol.join_from_welcome_bytes(&welcome_bytes).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);
    let epoch_before = alice.current_epoch(&channel_id).unwrap();

    nest.fail_channel_sends(true);
    backend
        .remove_participant(thread_id.clone(), fauna_addr("bob", bob_actor))
        .await
        .expect_err("send fault must fail the remove");
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "a failed send must not advance the epoch (Rule 1)"
    );
    assert!(
        alice
            .find_leaf_by_identity(&channel_id, &bob_actor)
            .is_some(),
        "bob is still a member after the failed remove"
    );

    // The group is fully intact: alice still talks to bob at the old epoch.
    nest.fail_channel_sends(false);
    backend
        .send(
            &fauna_mls_thread(thread_id.clone(), vec![]),
            &ComposeState {
                body_draft: "still here".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("send at the unadvanced epoch ok");
    let app_bytes = match ChannelEnvelope::from_bytes(
        nest.sent_envelopes(&channel_id.to_string()).last().unwrap(),
    )
    .unwrap()
    {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application"),
    };
    let bob_msg = bob
        .decrypt(&channel_id, &app_bytes)
        .expect("bob still decrypts — nothing was merged by the failed remove");
    assert!(matches!(bob_msg.body, ChannelMessageBody::Text(t) if t == "still here"));

    // Retry: the remove lands, carol ratchets, bob falls off the epoch.
    backend
        .remove_participant(thread_id.clone(), fauna_addr("bob", bob_actor))
        .await
        .expect("retry succeeds");
    assert!(
        alice.current_epoch(&channel_id).unwrap() > epoch_before,
        "the accepted retry merged"
    );
    let commit_bytes = match ChannelEnvelope::from_bytes(
        nest.sent_envelopes(&channel_id.to_string()).last().unwrap(),
    )
    .unwrap()
    {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("remove must post a Commit envelope"),
    };
    carol
        .process_commit(&channel_id, &commit_bytes)
        .expect("carol processes the retry's remove commit");
    assert_eq!(
        carol.current_epoch(&channel_id).unwrap(),
        alice.current_epoch(&channel_id).unwrap(),
        "remaining members converge on the retried remove"
    );
}

/// **Rule 2 heal (rewind + re-walk on `FutureEpochCommit`)** — `devices.md`
/// § Cross-device MLS group-state sync. A device whose ingest cursor skipped a
/// bridging commit (an un-processable hole) meets every later commit as
/// `FutureEpochCommit` and is permanently stranded — MLS members cannot skip
/// epochs. The heal: `poll_inbound_conv` rewinds its in-flight cursor to 0 once
/// per channel per session and re-walks the log in the same call — the skipped
/// commit applies, previously-undecryptable messages decrypt, and the device
/// lands on the head epoch. A second tear on the same channel must NOT rewind
/// again (an un-processable commit would otherwise loop full re-walks forever).
#[tokio::test]
async fn future_epoch_commit_heals_by_rewind_and_rewalk_once_per_session() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();
    let channel_hex = channel_id.to_string();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    // Materialize + bind the thread exactly as the production restore leg does.
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);
    let epoch_stale = alice.current_epoch(&channel_id).unwrap();

    // Bob advances the group twice, speaking in between: seq1 = the bridging
    // commit alice's cursor will skip, seq2 = a message sealed past it, seq3 =
    // the commit alice will meet as FutureEpochCommit, seq4 = a head message.
    let bob_commit = || {
        let cb = bob.self_update(&channel_id).unwrap();
        bob.merge_pending_commit(&channel_id).unwrap();
        let env = ChannelEnvelope::Commit(cb).to_bytes().unwrap();
        let nest = nest.clone();
        let hex = channel_hex.clone();
        async move { nest.channel_send(hex, env, None, vec![]).await.unwrap() }
    };
    let bob_text = |text: &str, sequence: u64| {
        let ct = bob
            .encrypt(
                &channel_id,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text(text.into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        let env = ChannelEnvelope::Application(ct).to_bytes().unwrap();
        let nest = nest.clone();
        let hex = channel_hex.clone();
        async move { nest.channel_send(hex, env, None, vec![]).await.unwrap() }
    };
    bob_commit().await; // seq 1: the bridging commit
    bob_text("bridge me", 1).await; // seq 2: sealed at the post-bridge epoch
    bob_commit().await; // seq 3: alice meets this as FutureEpochCommit
    bob_text("at head", 2).await; // seq 4: sealed at the head epoch

    // Torn state: alice's cursor starts PAST the bridging commit.
    let mut after = 1i64;
    let ingested = poll_inbound_conv(&backend, &manager, &channel_id, &mut after, 0)
        .await
        .expect("poll ok")
        .ingested;

    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        bob.current_epoch(&channel_id).unwrap(),
        "the heal re-walked the log and reached the head epoch (was stranded at {epoch_stale})"
    );
    assert_eq!(
        ingested, 2,
        "both of bob's messages decrypted on the re-walk"
    );
    assert_eq!(after, 4, "the cursor ends at the head of the log");
    let detail = manager.thread_detail(thread_id).expect("thread");
    for text in ["bridge me", "at head"] {
        assert!(
            detail.messages.iter().any(|m| m.body == text),
            "{text:?} ingested by the heal"
        );
    }

    // A SECOND tear on the same channel: the rewind is spent, so the poll must
    // NOT re-walk again — it stays loud-but-stranded (relaunch/resync recovers).
    let healed_epoch = alice.current_epoch(&channel_id).unwrap();
    bob_commit().await; // seq 5: skipped again below
    bob_text("past the second tear", 3).await; // seq 6
    bob_commit().await; // seq 7: met as FutureEpochCommit again
    let mut after2 = 5i64;
    poll_inbound_conv(&backend, &manager, &channel_id, &mut after2, 0)
        .await
        .expect("poll ok");
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        healed_epoch,
        "the once-per-session guard held: no second rewind on this channel"
    );
}

/// Renaming a group posts an encrypted **application** message (not a Commit)
/// carrying `GroupMeta::NameChanged` — group metadata rides the same sealed
/// channel as chat, so the new name is end-to-end encrypted. A peer decrypts it
/// back to the new label.
#[tokio::test]
async fn rename_posts_encrypted_namechanged_app_message() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread_id = ThreadId("g-1".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .rename(thread_id, "Team Fauna".into())
        .await
        .expect("rename ok");

    // No Commit, no Welcome — rename is a plain app message on the channel.
    assert!(nest.welcomes().is_empty(), "rename delivers no welcome");
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one app message posted");
    let app_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("rename must post an Application envelope"),
    };
    let decrypted = bob
        .decrypt(&channel_id, &app_bytes)
        .expect("bob decrypts rename");
    assert!(
        matches!(decrypted.body, ChannelMessageBody::GroupMeta(GroupMetaMessage::NameChanged(ref n)) if n == "Team Fauna"),
        "body is the NameChanged metadata, got {:?}",
        decrypted.body
    );
}

/// The inbound driver applies a decrypted `GroupMeta::NameChanged` as a thread
/// **rename** on the bound thread (not as a chat bubble): the group name change
/// propagates to peers through the same channel feed as messages.
#[tokio::test]
async fn inbound_driver_applies_namechanged_as_rename() {
    // Alice's group with Bob; Alice renames it through the backend.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend.bind_channel(ThreadId("a-1".into()), channel_id);
    alice_backend
        .rename(ThreadId("a-1".into()), "Renamed Group".into())
        .await
        .expect("alice rename");

    // Bob's manager + backend; seed his thread (participant-keyed) and bind it.
    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    // Drive inbound: the rename app message applies as a rename, not a bubble.
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(ingested, 0, "a rename is not a message bubble");

    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    assert_eq!(
        detail.label, "Renamed Group",
        "thread renamed from the feed"
    );
    assert_eq!(
        detail.messages.len(),
        1,
        "only the setup bubble — rename added no message"
    );
}

// ── the in-group succession statement (identity-succession.md § Propagation) ─

/// A [`fauna_conversations::backend::SuccessionWitness`] that runs the real
/// § The succession statement verification rule against the one chain head it
/// independently knows — the cached-`Profile.recovery_head` consumer in
/// miniature. Anything that fails the rule stays a claim (`None`).
struct HeadWitness(fauna_core::recovery::ChainHead);

#[async_trait]
impl fauna_conversations::backend::SuccessionWitness for HeadWitness {
    async fn verify(
        &self,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) -> Option<fauna_core::recovery::VerifiedSuccession> {
        statement.verify(&self.0).ok()?;
        Some(fauna_core::recovery::VerifiedSuccession {
            old_actor_id: statement.statement.old_actor_id,
            new_actor_id: statement.statement.new_actor_id,
            seq: statement.statement.seq,
            chain_head: self.0,
        })
    }
}

/// A witness holding no anchor until [`Self::arm`] — `verify` refuses (the
/// no-anchor degrade) before the head arrives and runs the real § The
/// succession statement rule after: `ChainWitness` across a peer-anchor
/// harvest, in miniature. What `arm` models is the harvest seeding the store
/// on its own read-path schedule — never a fetch the statement asked for.
#[derive(Default)]
struct SwitchWitness {
    head: Mutex<Option<fauna_core::recovery::ChainHead>>,
    /// Every call this witness received, **in order** — a real witness caches
    /// what the anchor store held, so "the seed was announced" and
    /// "it was announced before the verify that must see it" are two different
    /// facts and only the order tells them apart.
    calls: Mutex<Vec<&'static str>>,
}

impl SwitchWitness {
    fn arm(&self, head: fauna_core::recovery::ChainHead) {
        *self.head.lock().unwrap() = Some(head);
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl fauna_conversations::backend::SuccessionWitness for SwitchWitness {
    async fn anchor_seed_landed(&self) {
        self.calls.lock().unwrap().push("seed");
    }

    async fn harvest_settled(&self, _actor: &ActorId) {
        self.calls.lock().unwrap().push("settled");
    }

    async fn verify(
        &self,
        statement: fauna_core::recovery::SignedIdentitySuccession,
    ) -> Option<fauna_core::recovery::VerifiedSuccession> {
        self.calls.lock().unwrap().push("verify");
        let head = (*self.head.lock().unwrap())?;
        statement.verify(&head).ok()?;
        Some(fauna_core::recovery::VerifiedSuccession {
            old_actor_id: statement.statement.old_actor_id,
            new_actor_id: statement.statement.new_actor_id,
            seq: statement.statement.seq,
            chain_head: head,
        })
    }
}

/// Everything the statement tests need: alice's group with bob, bob's manager
/// and backend with a bound, participant-seeded thread, and a genuine
/// `SignedIdentitySuccession` for alice → a fresh successor identity.
struct SuccessionSetup {
    alice: Arc<MlsEngine>,
    channel_id: ChannelId,
    nest: Arc<MockNest>,
    bob_manager: Arc<ConversationsManager>,
    bob_backend: Arc<FaunaMlsBackend>,
    bob_thread_id: ThreadId,
    signed: fauna_core::recovery::SignedIdentitySuccession,
    head: fauna_core::recovery::ChainHead,
    successor: ActorId,
    /// The successor's own engine — the half that authors **remove-old**, since
    /// MLS forbids committing one's own removal. Built but not yet a member:
    /// [`run_add_successor`] is what seats it, so a test that wants the
    /// ceremony's midpoint asks for it and a test about a group the ceremony
    /// never reached simply does not.
    successor_engine: Arc<MlsEngine>,
}

/// Run the ceremony's **first** half into the fixture's group: alice's old leaf
/// commits add-successor and the successor joins from the Welcome, exactly as
/// `fauna_client_recovery::group_sweep` drives it. The commit goes on the
/// channel log, so the member under test folds it in through its own poll.
///
/// This is the state every honest statement arrives into — the statement rides
/// *alongside* the add (`fauna_mls::succession`), so the successor is seated
/// and the predecessor is not yet gone.
async fn run_add_successor(
    alice: &MlsEngine,
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    nest: &MockNest,
) {
    let key_package = successor_engine
        .generate_key_packages(1)
        .expect("the successor mints a key package")
        .remove(0);
    let added = fauna_mls::succession::commit_add_successor(alice, channel_id, &key_package)
        .expect("the old leaf commits add-successor");
    successor_engine
        .join_from_welcome(added.welcome)
        .expect("the successor joins from the Welcome");
    let envelope = ChannelEnvelope::Commit(added.commit_bytes)
        .to_bytes()
        .unwrap();
    nest.channel_send(channel_id.to_string(), envelope, None, vec![])
        .await
        .expect("the add-successor commit goes on the log");
}

/// Run the ceremony's **second** half: the successor's new leaf commits
/// remove-old, which is what completes the roster pair in this group. Requires
/// [`run_add_successor`] to have seated it.
async fn run_remove_old(
    successor_engine: &MlsEngine,
    channel_id: &ChannelId,
    nest: &MockNest,
    old_actor: &ActorId,
) {
    let commit = fauna_mls::succession::commit_remove_old(successor_engine, channel_id, old_actor)
        .expect("the successor commits remove-old");
    let envelope = ChannelEnvelope::Commit(commit).to_bytes().unwrap();
    nest.channel_send(channel_id.to_string(), envelope, None, vec![])
        .await
        .expect("the remove-old commit goes on the log");
}

fn succession_receive_setup() -> SuccessionSetup {
    let alice_kp = ActorKeypair::from_secret([11u8; 32]);
    let successor_kp = ActorKeypair::from_secret([22u8; 32]);
    let successor_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([22u8; 32])).unwrap());
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([11u8; 32])).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let recovery = fauna_core::recovery::RecoveryKey::generate();
    let statement = fauna_core::recovery::IdentitySuccession {
        old_actor_id: alice_kp.actor_id(),
        new_actor_id: successor_kp.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(
            &recovery,
            successor_kp.signing_key(),
            Some(alice_kp.signing_key()),
        )
        .expect("the fixture statement signs");
    let head = fauna_core::recovery::ChainHead::new(recovery.public(), 1);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor: successor_kp.actor_id(),
        successor_engine,
    }
}

/// Post `signed` into the channel as the `GroupMeta::Succession` app message,
/// sealed by alice's engine (any member may carry the statement — the
/// signatures, not the transport sender, are the authority).
async fn post_statement(
    alice: &MlsEngine,
    channel_id: &ChannelId,
    nest: &MockNest,
    signed: &fauna_core::recovery::SignedIdentitySuccession,
) {
    let bytes = fauna_core::encoding::canonical_encode(signed).unwrap();
    let message = ChannelMessage {
        sender: alice.identity_actor_id(),
        sequence: 9,
        channel_epoch: alice.current_epoch(channel_id).unwrap(),
        body: ChannelMessageBody::GroupMeta(GroupMetaMessage::Succession(bytes)),
        timestamp: Timestamp::now(),
    };
    let envelope = alice.encrypt_to_envelope(channel_id, &message).unwrap();
    nest.channel_send(channel_id.to_string(), envelope, None, vec![])
        .await
        .expect("post the statement");
}

/// A verified statement re-points the thread's participant row old→new — the
/// continuity render, in place of "a stranger arrived": same position, no
/// bubble, idempotent on re-delivery.
#[tokio::test]
async fn inbound_driver_repoints_participant_on_a_verified_succession() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

    // The whole ceremony, in the order the sweep drives it: add-successor,
    // the statement between the two commits, then remove-old. The re-point is
    // earned by the **pair** landing in this group, not by the statement
    // verifying (`succession-propagation.md` § Propagation → *MLS groups*).
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;

    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(
        ingested, 0,
        "a succession statement is not a message bubble"
    );

    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    let actor_of = |a: &TypedAddress| match a {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    };
    assert!(
        detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(successor)),
        "the successor holds alice's row: {:?}",
        detail.participants
    );
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(alice_actor)),
        "no stranger-plus-ghost pair — the old identity's row was re-pointed, \
         not duplicated: {:?}",
        detail.participants
    );
}

/// **The roster-pair gate** (`succession-propagation.md` § Propagation → *MLS
/// groups (per group)*): a verified statement replayed into a group where the
/// ceremony never ran re-points **nothing**.
///
/// A true statement is public — it rides the chain — so any member may carry
/// one into any group, and this arm's own comment already says the transport
/// sender is not the authority. Verification alone therefore proves the
/// succession *happened*, never that it happened **here**. Without the gate a
/// seed thief still holding the old leaf in a group the sweep never reached
/// replays the statement there and every member renders the rightful
/// successor while the thief stays seated — the "ceremony did not finish"
/// condition (`critical-alerts.md` § Feeders) painted as continuity, which is
/// the one rendering that hides it.
///
/// Here alice is still seated and the successor holds no leaf: the pair is
/// absent, so the row stands unchanged.
#[tokio::test]
async fn a_replayed_statement_repoints_nothing_where_the_ceremony_never_ran() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        ..
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

    // No add-successor commit was ever applied here — the group still holds
    // alice's leaf and has never held the successor's.
    let roster = bob_backend.engine().group_members(&channel_id);
    assert!(
        roster.contains(&alice_actor) && !roster.contains(&successor),
        "fixture precondition: the ceremony never ran in this group"
    );

    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    let actor_of = |a: &TypedAddress| match a {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    };
    assert!(
        detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(alice_actor)),
        "alice's row stands — she is still seated in this group: {:?}",
        detail.participants
    );
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(successor)),
        "the successor holds no leaf here, so nothing renders them: {:?}",
        detail.participants
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.seen, 1, "the arm ran");
    assert_eq!(counts.repointed, 0, "and re-pointed nothing");
}

/// The re-point is **visible to a driver**, and via the one field that can see
/// it — `data.conversation_threads[].participant_actor_ids`.
///
/// The sibling test above reads `ThreadDetail::participants`, which no e2e
/// driver can reach. This one asserts the same fact through the serialized
/// state contract a tier_3 journey actually polls
/// (`state_json::conversation_threads_json`), so the member-side journey is
/// pinned against the shape it reads rather than against an in-process twin of
/// it.
///
/// ⚠ **The display assertion is not decoration — it is the reason this field
/// exists.** The re-point keeps the handle (the nest moved it to the successor
/// inside the succession transaction), so `thread-member-chip[i]`'s text is
/// *identical* before and after. A journey asserting continuity through the
/// chip would pass just as green against a client that re-pointed nothing,
/// which is this file's recurring "right outcome through the wrong gate".
#[tokio::test]
async fn the_repoint_is_readable_through_the_state_contract_a_driver_polls() {
    use fauna_conversations::state_json::conversation_threads_json;

    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

    let ids_of = |rows: &serde_json::Value| -> Vec<Option<String>> {
        rows.as_array()
            .expect("the contract is an array")
            .iter()
            .find(|r| r["thread_id"].as_str() == Some(bob_thread_id.0.as_str()))
            .unwrap_or_else(|| panic!("bob's thread must serialize; rows={rows:?}"))
            ["participant_actor_ids"]
            .as_array()
            .expect("participant_actor_ids is an array")
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect()
    };
    let displays_of = || -> Vec<String> {
        bob_manager
            .thread_detail(bob_thread_id.clone())
            .expect("thread")
            .participant_displays
    };

    let before = conversation_threads_json(&bob_manager);
    let ids_before = ids_of(&before);
    let displays_before = displays_of();
    let alice_at = ids_before
        .iter()
        .position(|id| id.as_deref() == Some(alice_actor.to_hex().as_str()))
        .unwrap_or_else(|| {
            panic!("alice must hold a serialized slot before the statement: {ids_before:?}")
        });

    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    let ids_after = ids_of(&conversation_threads_json(&bob_manager));
    assert_eq!(
        ids_after.get(alice_at).and_then(Option::as_deref),
        Some(successor.to_hex().as_str()),
        "the successor must hold alice's ORIGINAL slot — same index, since the \
         re-point is in place and a journey pairs the id with the chip that \
         renders it. before={ids_before:?} after={ids_after:?}"
    );
    assert!(
        !ids_after
            .iter()
            .any(|id| id.as_deref() == Some(alice_actor.to_hex().as_str())),
        "the retired identity must not survive anywhere in the row — a \
         stranger-plus-ghost pair is what re-pointing exists to avoid: \
         {ids_after:?}"
    );
    assert_eq!(
        displays_of(),
        displays_before,
        "the rendered chips must be UNCHANGED across the re-point (the handle \
         moved with the account). If this ever fails, the chip became able to \
         witness continuity on its own — but until it does, a journey asserting \
         on chip text asserts nothing"
    );
}

/// The receive-side tally separates the three states that render identically.
///
/// Every failure on this path leaves the participant row exactly as it was, so
/// from outside the process "the statement never got here", "no witness was
/// wired" and "the witness refused it" are the *same observation* — and the
/// `debug!` lines that would tell them apart are unreadable in production (no
/// app installs a tracing subscriber). That indistinguishability is not
/// hypothetical: it is what made the member path's first full-stack run a
/// 120-second timeout with no evidence in it. This pins that each arm now
/// reports itself, which is the only reason the report is worth carrying.
#[tokio::test]
async fn the_statement_tally_tells_the_silent_arms_apart() {
    // Arm 1 — the statement never arrives. The distinguishing count is `seen`,
    // and it must be zero even though the poll ran: a caller reading a nonzero
    // `seen` has *proved* the body reached the arm, which is what exonerates
    // the whole inbound path below it.
    {
        let SuccessionSetup {
            channel_id,
            bob_manager,
            bob_backend,
            head,
            ..
        } = succession_receive_setup();
        bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");

        assert_eq!(
            bob_backend.succession_statement_counts(),
            Default::default(),
            "a poll that met no statement must report none — a tally that \
             counted anything here would make the decisive `seen == 0` reading \
             worthless"
        );
    }

    // Arm 2 — the body arrives with no witness wired. Correct behaviour
    // (degrade to the bare add) and a silent one; on an app that believes it
    // registered a witness this count is the entire bug.
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            signed,
            ..
        } = succession_receive_setup();

        post_statement(&alice, &channel_id, &nest, &signed).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");

        let counts = bob_backend.succession_statement_counts();
        assert_eq!(counts.seen, 1);
        assert_eq!(counts.no_witness, 1);
        assert_eq!(counts.repointed, 0);
    }

    // Arm 3 — verified and re-pointed. Counted so a green run proves the path
    // RAN, rather than proving that nothing happened to notice.
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            signed,
            head,
            successor_engine,
            ..
        } = succession_receive_setup();
        let alice_actor = alice.identity_actor_id();
        bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

        run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
        post_statement(&alice, &channel_id, &nest, &signed).await;
        run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");

        let counts = bob_backend.succession_statement_counts();
        assert_eq!(counts.seen, 1);
        assert_eq!(counts.no_witness, 0);
        assert_eq!(counts.undecodable, 0);
        assert_eq!(
            counts.repointed, 1,
            "the success arm must be counted too, or a report can only ever \
             describe failures"
        );
        assert_eq!(
            counts.not_in_this_group, 0,
            "the ceremony ran HERE, so the roster-pair gate never fired"
        );
    }

    // Arm 4 — verified, and refused by the roster-pair gate: the statement
    // replayed into a group the ceremony never reached. It renders exactly
    // like every arm above (nothing moves), so without its own count it is
    // indistinguishable from "the witness refused" — and the two want opposite
    // responses, since this one means a succeeded credential may still be
    // seated somewhere (`critical-alerts.md` § Feeders).
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            signed,
            head,
            ..
        } = succession_receive_setup();
        bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

        post_statement(&alice, &channel_id, &nest, &signed).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");

        let counts = bob_backend.succession_statement_counts();
        assert_eq!(counts.seen, 1);
        assert_eq!(counts.no_witness, 0);
        assert_eq!(counts.repointed, 0, "the gate held the re-point");
        assert_eq!(
            counts.not_in_this_group, 1,
            "and named WHY, which is the whole reason this arm is counted \
             apart from a witness refusal"
        );
        assert_eq!(
            counts.parked, 0,
            "a group the ceremony never reached holds nothing: when it does \
             reach here, the sweep posts the statement again beside its add"
        );
    }

    // Arm 5 — verified, and HELD by the gate at the ceremony's own midpoint:
    // the successor is seated, the predecessor's leaf has not gone yet. This
    // is the honest path, so it must park rather than degrade.
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            signed,
            head,
            successor_engine,
            ..
        } = succession_receive_setup();
        bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

        run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
        post_statement(&alice, &channel_id, &nest, &signed).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");

        let counts = bob_backend.succession_statement_counts();
        assert_eq!(counts.seen, 1);
        assert_eq!(counts.repointed, 0, "remove-old has not landed yet");
        assert_eq!(counts.awaiting_remove_old, 1);
        assert_eq!(
            counts.parked, 1,
            "held for the remove-old commit, not dropped — a consumed MLS \
             application message cannot be re-decrypted, so this delivery is \
             the only copy this session will ever hold"
        );
        assert_eq!(counts.not_in_this_group, 0);
    }
}

/// The harvest-race convergence (`identity-succession.md` § The succession
/// statement → *the peer-profile harvest*, the parked-statement re-drive): a
/// statement the witness refused for want of an anchor is PARKED, and when the
/// harvest later seeds that peer's anchor — on its own read-path schedule — the
/// re-drive settles the parked statement through the ordinary verify and
/// re-points the row. Without this, the one delivery is the only chance the
/// mechanism ever gets: a consumed MLS application message cannot be
/// re-decrypted (forward secrecy), so "wait for a re-delivery" waits forever.
///
/// The arm() here stands for the harvest landing; the pin that the REAL
/// harvest triggers no verify-time fetch lives with the real `ChainWitness`
/// (`fauna-client-recovery`'s witness tests, which count dials and fetches).
#[tokio::test]
async fn a_refused_statement_is_parked_and_a_later_harvest_redrive_repoints_it() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    // The whole ceremony lands in this group — the statement in its real
    // position, between the two commits. The subject here is the ANCHOR wait,
    // so the roster pair must not be the thing still missing.
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    // Refused: the row still names the old identity, and the statement is
    // parked rather than dropped — parking is the only thing that keeps the
    // race from being terminal.
    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "before the anchor lands the claim stays a claim"
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.seen, 1);
    assert_eq!(
        counts.parked, 1,
        "a refused statement about a current participant must be parked — \
         dropping it makes the harvest race permanent"
    );
    assert_eq!(counts.repointed, 0);

    // The harvest lands (the witness can now anchor tier 1) and the re-drive
    // settles the parked statement: the row re-points with no new delivery.
    witness.arm(head);
    let redriven = redrive_parked_successions(&bob_backend, &bob_manager, &alice_actor).await;
    assert_eq!(redriven, 1, "the parked statement must be re-driven");
    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == successor)),
        "the re-drive re-points the row to the successor: {:?}",
        detail.participants
    );
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "the old identity's row is re-pointed in place, not duplicated"
    );
    assert_eq!(bob_backend.succession_statement_counts().repointed, 1);
    // **The order is the contract, not just the fact.** A witness that caches
    // what the anchor store held answers the re-drive's verify from
    // that cache, so the seed must be announced *before* it — announcing after
    // would settle nothing and re-park a statement whose anchor is already at
    // rest, and no second harvest event ever comes for a peer already seeded.
    // The settle rides between them: a seed is also a settle, and a witness
    // released BEFORE it re-read the store would settle on the very head the
    // seed just demoted (the harvest wait, same §).
    assert_eq!(
        witness.calls(),
        vec!["verify", "verify", "seed", "settled", "verify"],
        "the poll's refusal, the remove-old commit's own re-drive attempt \
         (the roster pair completed, so the commit arm re-tried and the \
         witness refused again — still no anchor), then the harvest re-drive: \
         seed announced, the settle after it, then re-verified"
    );

    // Settled means settled: a second re-drive finds nothing parked.
    assert_eq!(
        redrive_parked_successions(&bob_backend, &bob_manager, &alice_actor).await,
        0,
        "a verified parked statement is consumed, not re-verified forever"
    );
    // …and it announced the seed anyway. This is the ordinary case, not the
    // edge one: the sweep usually wins the race, so most seeds land with
    // nothing parked at all — and a witness that only heard about the seeds
    // that happened to have a parked statement waiting would keep serving a
    // stale "no anchor held" view for every one of the others.
    assert_eq!(
        witness.calls(),
        vec![
            "verify", "verify", "seed", "settled", "verify", "seed", "settled"
        ],
        "the seed signal is unconditional — the parked set is consulted after it"
    );
}

/// The harvest wait's release (`identity-succession.md` § The succession
/// statement → *the harvest wait*): a statement the witness held back because
/// this session's harvest of the peer was still owed is PARKED like any other
/// refusal, and the sweep settling that peer **without seeding anything** —
/// the honest majority: the peer never rotated — re-drives it through the
/// ordinary verify. Before the wait, only a landed seed re-drove, so this
/// statement would have sat parked until the session ended.
///
/// And it announces the settle ONLY: no seed landed, and a witness that reads
/// its anchor store once per seed generation must not be told otherwise, or
/// the read bound degrades to one read per settled roster peer.
#[tokio::test]
async fn a_settle_that_seeded_nothing_redrives_the_parked_statement_and_announces_no_seed() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    // The whole ceremony lands in this group — the statement in its real
    // position, between the two commits. The subject here is the ANCHOR wait,
    // so the roster pair must not be the thing still missing.
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(bob_backend.succession_statement_counts().parked, 1);

    // The sweep settles alice with nothing new; the witness's wait is over
    // (`arm` stands for that here — the real release is pinned with the real
    // `ChainWitness`, in `fauna-client-recovery`'s witness tests).
    witness.arm(head);
    let redriven = settle_parked_successions(&bob_backend, &bob_manager, &alice_actor).await;
    assert_eq!(redriven, 1, "the settle must re-drive what waited on it");
    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == successor)),
        "the row re-points with no new delivery: {:?}",
        detail.participants
    );
    assert_eq!(
        witness.calls(),
        vec!["verify", "verify", "settled", "verify"],
        "the poll's refusal, the remove-old commit's own re-drive attempt, \
         then the settle announced before the re-verify — and never a seed"
    );
}

/// **The gate converges** — the honest ceremony delivered across TWO polls,
/// which is the case the roster-pair gate would otherwise break.
///
/// The statement rides *alongside the add* and the remove-old commit lands
/// after it (`fauna_mls::succession`), so at the moment the statement arrives
/// the predecessor is ALWAYS still seated: the gate holds every honest
/// succession on its way through. Holding is only safe because a held
/// statement is re-driven when the remove-old commit arrives — a consumed MLS
/// application message cannot be re-decrypted, so that one delivery is the
/// only copy this session will ever hold, and a gate that merely dropped it
/// would mean no member ever renders continuity again.
///
/// Splitting the walk is the point: in a single poll the two commits and the
/// statement could settle by accident of ordering. Here poll 1 ends with the
/// statement parked and the row untouched, and only the commit arriving in
/// poll 2 completes the pair.
#[tokio::test]
async fn the_held_statement_settles_when_the_remove_old_commit_arrives_in_a_later_poll() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));
    let actor_of = |a: &TypedAddress| match a {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    };
    let seats = || -> Vec<Option<ActorId>> {
        bob_manager
            .thread_detail(bob_thread_id.clone())
            .expect("thread")
            .participants
            .iter()
            .map(actor_of)
            .collect()
    };

    // Poll 1 — the add and the statement. The ceremony is half done.
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    assert!(
        seats().contains(&Some(alice_actor)),
        "mid-ceremony the row still names the predecessor — she is still \
         seated in this group: {:?}",
        seats()
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.awaiting_remove_old, 1, "held, not degraded");
    assert_eq!(
        counts.parked, 1,
        "and held onto — this copy is the only one"
    );
    assert_eq!(counts.repointed, 0);

    // Poll 2 — remove-old lands. Nothing re-delivers the statement; the commit
    // re-drive is the whole mechanism under test.
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    assert!(
        seats().contains(&Some(successor)),
        "the pair completed, so the row re-points: {:?}",
        seats()
    );
    assert!(
        !seats().contains(&Some(alice_actor)),
        "in place — no stranger-plus-ghost pair: {:?}",
        seats()
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(
        counts.seen, 1,
        "ONE delivery did all of this — the statement was never re-sent"
    );
    assert_eq!(counts.repointed, 1);

    // Consumed: a further commit finds nothing left to re-drive, so the row
    // cannot be re-pointed twice off one parked copy.
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(bob_backend.succession_statement_counts().repointed, 1);
}

/// **The gate covers the HARVEST re-drive too** — the second door into the
/// re-point, and the one a fix written only at the arrival arm leaves open.
///
/// The two arms park for different reasons and the orders interleave: here the
/// witness refuses the replayed statement first (no anchor yet), so it parks on
/// the ordinary refusal path — and parks legitimately, since the predecessor
/// *does* hold a row in this thread, which is the bound parking is capped by.
/// The anchor then lands and the harvest re-drives it. At that moment the
/// statement verifies, and nothing about the group has changed: the ceremony
/// still never ran here. A re-drive that re-points on the verification alone
/// reintroduces the whole defect one delivery later, which is why the gate
/// lives in `settle_verified_succession` rather than at either call site.
#[tokio::test]
async fn the_harvest_redrive_does_not_repoint_a_group_the_ceremony_never_ran_in() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        ..
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    // No ceremony in this group — only the statement, replayed.
    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(
        bob_backend.succession_statement_counts().parked,
        1,
        "the witness refused it, and the predecessor holds a row here, so it \
         parks on the ordinary refusal path"
    );

    // The anchor lands. The statement now verifies — and still must not move
    // the row, because the group's roster never changed.
    witness.arm(head);
    let redriven = redrive_parked_successions(&bob_backend, &bob_manager, &alice_actor).await;
    assert_eq!(
        redriven, 0,
        "a verified statement is not a re-point: this group has no pair"
    );

    let detail = bob_manager
        .thread_detail(bob_thread_id.clone())
        .expect("thread");
    let actor_of = |a: &TypedAddress| match a {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    };
    assert!(
        detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(alice_actor)),
        "the predecessor's row stands — she is still seated here: {:?}",
        detail.participants
    );
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| actor_of(p) == Some(successor)),
        "and the successor, who holds no leaf here, renders nowhere: {:?}",
        detail.participants
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.repointed, 0);
    assert_eq!(
        counts.not_in_this_group, 1,
        "the re-drive names the arm that refused it, exactly as the arrival \
         arm does"
    );
}

/// **The gate covers the COMMIT re-drive too** — the third door into the
/// re-point, and the one both of the pins above leave open.
///
/// [`redrive_parked_in_thread`] fires on **every** `Advanced` commit, not only
/// the remove-old one that completes a ceremony: any membership move in the
/// group re-drives whatever that thread is holding. So a statement replayed
/// into a group the ceremony never ran in — parked here on the ordinary
/// refusal path, and parked legitimately, since the predecessor holds a row —
/// meets this door the next time any member is added, by which time its anchor
/// may well have landed. A re-drive that re-pointed on the verification alone
/// would rebuild the whole defect behind an unrelated commit, one delivery and
/// one stranger's join later.
///
/// The second half is the same door's other duty: refusing is **holding**, not
/// dropping. When the ceremony does reach this group the held copy is what
/// renders continuity — there is no second delivery to fall back on.
#[tokio::test]
async fn the_commit_redrive_does_not_repoint_a_group_the_ceremony_never_ran_in() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        bob_thread_id,
        signed,
        head,
        successor,
        successor_engine,
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());
    let actor_of = |a: &TypedAddress| match a {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    };
    let seats = || -> Vec<Option<ActorId>> {
        bob_manager
            .thread_detail(bob_thread_id.clone())
            .expect("thread")
            .participants
            .iter()
            .map(actor_of)
            .collect()
    };

    // No ceremony in this group — only the statement, replayed.
    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(
        bob_backend.succession_statement_counts().parked,
        1,
        "the witness refused it, and the predecessor holds a row here, so it \
         parks on the ordinary refusal path"
    );

    // The anchor lands — the statement would verify now...
    witness.arm(head);

    // ...and an UNRELATED membership commit moves this group's roster: carol
    // joins, which is an `Advanced` outcome and so drives the commit re-drive.
    // The roster it moved still carries no pair.
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol_actor = carol.identity_actor_id();
    let carol_kp = carol
        .generate_key_packages(1)
        .expect("carol mints a key package")
        .remove(0);
    let (commit_bytes, _welcome) = alice
        .add_member(&channel_id, &carol_kp)
        .expect("alice adds carol");
    let envelope = ChannelEnvelope::Commit(commit_bytes).to_bytes().unwrap();
    nest.channel_send(channel_id.to_string(), envelope, None, vec![])
        .await
        .expect("the unrelated add commit goes on the log");
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    // The commit really did land — otherwise the re-drive never ran and the
    // assertions below would hold for the wrong reason.
    let roster = bob_backend.engine().group_members(&channel_id);
    assert!(
        roster.contains(&carol_actor),
        "fixture: the unrelated commit advanced this group's roster"
    );
    assert!(
        roster.contains(&alice_actor) && !roster.contains(&successor),
        "and it left the succession pair exactly as absent as it was"
    );
    assert!(
        seats().contains(&Some(alice_actor)),
        "the predecessor's row stands — she is still seated here: {:?}",
        seats()
    );
    assert!(
        !seats().contains(&Some(successor)),
        "and the successor, who holds no leaf here, renders nowhere: {:?}",
        seats()
    );
    assert_eq!(
        bob_backend.succession_statement_counts().repointed,
        0,
        "a verified statement is not a re-point: an unrelated commit is not \
         this group's ceremony"
    );

    // Refused, not dropped: the ceremony reaches this group at last, and the
    // held copy is what renders continuity — nothing re-delivers it.
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert!(
        seats().contains(&Some(successor)) && !seats().contains(&Some(alice_actor)),
        "the pair completed, so the held statement re-points in place: {:?}",
        seats()
    );
    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.repointed, 1);
    assert_eq!(
        counts.seen, 1,
        "ONE delivery did all of this — the statement was never re-sent"
    );
}

/// The parking bound: a statement naming an identity with no row in the bound
/// thread re-points nothing, so parking it would only hand an in-group forger
/// (who can mint statements with arbitrary `old_actor_id`s) a
/// per-statement memory and re-drive amplification. Parked entries are capped
/// by the roster because only roster rows can ever be re-pointed.
#[tokio::test]
async fn a_statement_naming_a_non_participant_is_not_parked() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        ..
    } = succession_receive_setup();
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    // A genuine, verifiable statement — about an identity that is not in the
    // thread. The bound is membership, not signature validity.
    let stranger = ActorKeypair::from_secret([33u8; 32]);
    let stranger_successor = ActorKeypair::from_secret([44u8; 32]);
    let recovery = fauna_core::recovery::RecoveryKey::generate();
    let statement = fauna_core::recovery::IdentitySuccession {
        old_actor_id: stranger.actor_id(),
        new_actor_id: stranger_successor.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(
            &recovery,
            stranger_successor.signing_key(),
            Some(stranger.signing_key()),
        )
        .expect("the stranger statement signs");

    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");

    let counts = bob_backend.succession_statement_counts();
    assert_eq!(counts.seen, 1);
    assert_eq!(
        counts.parked, 0,
        "a statement about a non-participant must not be parked"
    );
    witness.arm(fauna_core::recovery::ChainHead::new(recovery.public(), 1));
    assert_eq!(
        redrive_parked_successions(&bob_backend, &bob_manager, &stranger.actor_id()).await,
        0,
        "nothing was parked, so nothing is re-driven"
    );
}

/// The two degrade paths that must NOT re-point: no witness registered (the
/// session never wired one), and a witness that refuses the claim (here: the
/// statement re-signed under a key the member does not know — the shape a seed
/// thief can mint). Both render the bare add; neither trusts the bytes.
#[tokio::test]
async fn an_unverified_statement_repoints_nothing() {
    // Arm 1 — no witness.
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            bob_thread_id,
            signed,
            ..
        } = succession_receive_setup();
        post_statement(&alice, &channel_id, &nest, &signed).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");
        let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
        assert!(
            detail.participants.iter().any(
                |p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice.identity_actor_id())
            ),
            "without a witness the claim stays a claim: {:?}",
            detail.participants
        );
    }

    // Arm 2 — a witness holding a head the statement does not verify under:
    // the thief-minted-chain shape. The head names a DIFFERENT recovery key.
    {
        let SuccessionSetup {
            alice,
            channel_id,
            nest,
            bob_manager,
            bob_backend,
            bob_thread_id,
            signed,
            ..
        } = succession_receive_setup();
        let wrong_key = fauna_core::recovery::RecoveryKey::generate();
        bob_backend.set_succession_witness(Arc::new(HeadWitness(
            fauna_core::recovery::ChainHead::new(wrong_key.public(), 1),
        )));
        post_statement(&alice, &channel_id, &nest, &signed).await;
        let mut after_seq = 0i64;
        poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
            .await
            .expect("poll ok");
        let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
        assert!(
            detail.participants.iter().any(
                |p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice.identity_actor_id())
            ),
            "a statement the rule refuses must change nothing: {:?}",
            detail.participants
        );
    }
}

// ── Track D: manager async drivers (snapshot mutation + backend wire op) ─
//
// These exercise the client-facing single-method-per-action surface
// (`docs/goal/ui/conversations.md` § User actions) the linux app drives:
// a real `FaunaMlsBackend` is registered on a `ConversationsManager`, a thread
// is materialized + bound to its channel, and the async driver is asserted to
// (a) mutate the snapshot and (b) fire the captured MLS wire op on the MockNest.

/// `manager.rename_thread` relabels the snapshot **and** posts the encrypted
/// `NameChanged` app message that a peer decrypts back to the new label.
#[tokio::test]
async fn manager_rename_thread_mutates_snapshot_and_posts_wire_op() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", alice_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    manager
        .rename_thread(tid.clone(), "Lunch Crew".into())
        .await;

    // Snapshot relabeled.
    let detail = manager.thread_detail(tid).expect("thread");
    assert_eq!(detail.label, "Lunch Crew");

    // Wire op: one Application envelope carrying NameChanged, decryptable by Bob.
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one app message posted");
    let app_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("rename must post an Application envelope"),
    };
    let cm = bob.decrypt(&channel_id, &app_bytes).expect("bob decrypts");
    assert!(
        matches!(cm.body, ChannelMessageBody::GroupMeta(GroupMetaMessage::NameChanged(ref n)) if n == "Lunch Crew"),
        "got {:?}",
        cm.body
    );
}

/// `manager.remove_participant` drops the participant from the snapshot **and**
/// posts the MLS Commit that cuts the removed member off the epoch.
#[tokio::test]
async fn manager_remove_participant_mutates_snapshot_and_posts_commit() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
    carol.join_from_welcome_bytes(&welcome_bytes).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    manager
        .remove_participant(tid.clone(), fauna_addr("bob", bob_actor))
        .await;

    // Snapshot dropped Bob.
    let detail = manager.thread_detail(tid).expect("thread");
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == bob_actor)),
        "Bob removed from the snapshot roster"
    );

    // Wire op: one Commit; Carol ratchets, Bob is cut off.
    assert!(nest.welcomes().is_empty(), "removal delivers no welcome");
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one commit posted");
    let commit_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Commit(b) => b,
        _ => panic!("remove must post a Commit"),
    };
    carol
        .process_commit(&channel_id, &commit_bytes)
        .expect("carol processes the remove commit");
}

/// `manager.confirm_add_participant` on an in-place (bound) FaunaMls group adds
/// to the snapshot **and** posts the MLS Commit + Welcome for the new member.
#[tokio::test]
async fn manager_confirm_add_participant_in_place_group_commits_and_welcomes() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let dave_actor = dave.identity_actor_id();
    let dave_hex = hex::encode(dave_actor.0);

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &dave_hex,
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    // 2 participants ⇒ MlsGroup flavor ⇒ in-place add (no fork).
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    let before = manager.snapshot().threads.len();
    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("dave", dave_actor));
    let result = manager
        .confirm_add_participant()
        .await
        .expect("returns the thread id");

    // Snapshot: same thread (no fork), Dave added.
    assert_eq!(result, tid, "in-place group add does not fork");
    assert_eq!(manager.snapshot().threads.len(), before, "no new thread");
    let detail = manager.thread_detail(tid).expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == dave_actor)),
        "Dave added to the snapshot roster"
    );

    // Wire op: one Commit posted + one group Welcome delivered to Dave.
    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "one commit posted");
    assert!(matches!(
        ChannelEnvelope::from_bytes(&envelopes[0]).unwrap(),
        ChannelEnvelope::Commit(_)
    ));
    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1, "one welcome to the new member");
    assert_eq!(welcomes[0].recipient_hex, dave_hex);
    assert!(
        matches!(welcomes[0].kind, WelcomeChannelKind::Group { .. }),
        "in-place group add is a group welcome, got {:?}",
        welcomes[0].kind
    );
}

/// A **failed** in-place add must not leave the person shown as a participant.
/// `confirm_add_participant` mutates the snapshot before it fires the wire op,
/// so without a rollback a failure renders the exact inverse of the
/// "rendered truthfully as not-yet-shared" property `key-material-hierarchy.md`
/// § M2 requires of a half-added member.
#[tokio::test]
async fn manager_confirm_add_participant_rolls_back_the_snapshot_when_the_wire_op_fails() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let dave_actor = dave.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    // No key package seeded for dave ⇒ the wire op fails.
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("dave", dave_actor));
    manager.confirm_add_participant().await;

    let detail = manager.thread_detail(tid).expect("thread");
    assert!(
        !detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == dave_actor)),
        "a failed add must not leave dave listed in a group he is not in"
    );
    assert!(
        nest.welcomes().is_empty(),
        "sanity: the wire op really did fail"
    );

    // The OTHER half of the same failure: what the page says. The membership
    // ops feed the snapshot's `error`, which is the same `error-message`
    // element the send slot feeds, so the send-slot taxonomy governs it too
    // (`conversations.md` § Errors & edge cases). This failure is
    // `Internal("no key package available for <actor hex>")` — a diagnostic —
    // so the page must show the generic sentence, not the hex.
    let page_error = manager
        .snapshot()
        .error
        .expect("a failed membership wire op must surface on error-message");
    assert_eq!(
        page_error.key,
        "conversations.unified.error_add_participant"
    );
    let message = page_error
        .args
        .get("message")
        .expect("the reason rides {message}");
    assert_eq!(
        message,
        fauna_i18n::strings::error::send::GENERIC,
        "a diagnostic must reach the page as the generic sentence"
    );
    let dave_hex = hex::encode(dave_actor.0);
    assert!(
        !message.contains(&dave_hex) && !message.contains("key package"),
        "the raw diagnostic leaked onto error-message: {message:?}"
    );
}

/// The rollback undoes only what *this* gesture added. Re-adding someone
/// already in the thread is idempotent in the store, so a failing wire op on a
/// duplicate gesture must NOT evict the existing participant from the list.
#[tokio::test]
async fn manager_confirm_add_participant_rollback_never_evicts_an_existing_participant() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    // No key package for carol ⇒ the wire op fails — but carol is already a
    // listed participant, so the list must be untouched.
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("carol", carol_actor));
    manager.confirm_add_participant().await;

    let detail = manager.thread_detail(tid).expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == carol_actor)),
        "carol was already a participant — the rollback must not evict her"
    );
}

/// Rolling the list back makes a failed add **honest**; this makes it
/// **visible**. Before the page error the overlay just closed and nothing
/// happened — no message, no effect — which is exactly what a dropped command
/// looks like (`docs/goal/architecture/testing.md` point 11) and what
/// `conversations.md` § Errors & edge cases says must reach `error-message`.
/// The backend's own detail rides along as `{message}` so the refuse-to-guess
/// arm of the R4 heal ("roster unreadable", `mls-group-key-material.md` § M2)
/// reaches the user as the *specific, actionable* error that bullet requires,
/// not a generic failure.
#[tokio::test]
async fn manager_confirm_add_participant_surfaces_the_failure_on_the_page_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let dave_actor = dave.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    // No key package seeded for dave ⇒ the wire op fails.
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    assert!(
        manager.snapshot().error.is_none(),
        "sanity: no page error before the gesture"
    );

    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("dave", dave_actor));
    manager.confirm_add_participant().await;

    let error = manager
        .snapshot()
        .error
        .expect("a failed add must surface on the page's error-message");
    assert_eq!(error.key, "conversations.unified.error_add_participant");
    let detail = error.args.get("message").expect("the backend's own detail");
    assert!(
        !detail.is_empty(),
        "the reason must be actionable, not an empty placeholder"
    );
}

/// The error is a *gesture* outcome, not a sticky page mode: a later add that
/// succeeds must clear it, or the page keeps accusing the user of a failure
/// they already recovered from.
#[tokio::test]
async fn manager_confirm_add_participant_clears_a_stale_page_error_on_success() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let dave_actor = dave.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    // First gesture fails (dave has no key package on the nest yet).
    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("dave", dave_actor));
    manager.confirm_add_participant().await;
    assert!(
        manager.snapshot().error.is_some(),
        "sanity: the first gesture failed"
    );

    // Seed dave's key package, then retry — the recovery gesture the error
    // itself invites.
    nest.seed_keypackage(
        &hex::encode(dave_actor.0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("dave", dave_actor));
    manager.confirm_add_participant().await;

    assert!(
        manager.snapshot().error.is_none(),
        "a successful add must clear the previous failure's error"
    );
}

/// `remove_participant` swallowed its wire error exactly as the add did, and
/// with the same consequence: the chip disappears from the header while the
/// member is still in the group, and nothing says so. One page-level surface
/// covers every membership/label wire op, which is why the snapshot field is
/// general rather than add-specific.
#[tokio::test]
async fn manager_remove_participant_surfaces_the_failure_on_the_page_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let mallory = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();
    let mallory_actor = mallory.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    // Mallory is not a leaf of this group, so the MLS Remove has nothing to
    // remove and the wire op fails.
    manager
        .remove_participant(tid.clone(), fauna_addr("mallory", mallory_actor))
        .await;

    let error = manager
        .snapshot()
        .error
        .expect("a failed remove must surface on the page's error-message");
    assert_eq!(error.key, "conversations.unified.error_remove_participant");
}

/// The remove's own truthfulness half, the mirror of the add's rollback. An
/// `Err` out of `remove_participant` provably means the member is **still in
/// the group**: both routes merge only on an accepted send (the ungated one
/// clears the staged pending on failure, `fauna_mls.rs::evict_leaf_locked`; the
/// gated one merges inside the rebase loop), so hiding their chip would render
/// the same inverse-of-truth the add's rollback exists to prevent.
#[tokio::test]
async fn manager_remove_participant_puts_the_member_back_when_the_wire_op_fails() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();

    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, _welcome) = alice.create_group(&kps).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(
        channel_id.to_string(),
        vec![
            fauna_addr("bob", bob_actor),
            fauna_addr("carol", carol_actor),
        ],
    );
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    // Carol IS a leaf, so the eviction gets as far as the send — which fails.
    nest.fail_channel_sends(true);
    manager
        .remove_participant(tid.clone(), fauna_addr("carol", carol_actor))
        .await;

    let detail = manager.thread_detail(tid).expect("thread");
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == carol_actor)),
        "the remove failed, so carol is still in the group and must still be listed"
    );
    assert_eq!(
        manager
            .snapshot()
            .error
            .expect("and the failure must be visible")
            .key,
        "conversations.unified.error_remove_participant"
    );
}

/// `manager.confirm_add_participant` on a FaunaMls **1:1** forks a fresh group
/// thread in the snapshot and fires **no** wire op (the new group bootstraps
/// lazily on first `send`, Track B).
#[tokio::test]
async fn manager_confirm_add_participant_oneonone_forks_snapshot_only() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();

    let manager = ConversationsManager::new();
    // 1 participant ⇒ OneToOne flavor ⇒ add forks a new group.
    let tid =
        manager.materialize_conv_thread(channel_id.to_string(), vec![fauna_addr("bob", bob_actor)]);
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());

    manager.open_add_participant(tid.clone());
    manager.accept_add_participant_chip(fauna_addr("carol", carol_actor));
    let new_id = manager
        .confirm_add_participant()
        .await
        .expect("returns the forked thread id");

    // Snapshot: forked a new thread, selected it; original 1:1 untouched.
    assert_ne!(new_id, tid, "1:1 add forks a new thread");
    assert_eq!(manager.snapshot().threads.len(), 2, "fork added one thread");
    assert_eq!(manager.snapshot().selected_thread_id, Some(new_id));

    // No wire op: the fork bootstraps lazily on first send.
    assert_eq!(nest.total_sends(), 0, "fork posts nothing to the channel");
    assert!(nest.welcomes().is_empty(), "fork delivers no welcome");
}

// ── Track F: address resolution (recipient-picker rail probe) ──────────
//
// `resolve_address` is the async backend probe behind the recipient picker
// (`docs/goal/ui/conversations.md` § Where logic lives → "rail probe, MLS key
// lookup"; § User actions `recipient-picker-input` → `resolve_recipient`). The
// first, smallest-useful form: a 64-hex string IS the 32-byte actor key, so
// `TypedAddress::Fauna` needs no handle→actor lookup — a non-destructive
// `keypackage.count > 0` probe confirms the actor is a reachable Fauna peer.

/// A 64-hex actor id whose actor has published key packages resolves to a
/// `TypedAddress::Fauna` (rail = FaunaMls), carrying the typed hex as the handle
/// and the decoded 32 bytes as the `ActorId`.
#[tokio::test]
async fn resolve_address_actor_id_hex_with_keypackages_resolves() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    // Bob has a key package on the nest → reachable.
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    match backend.resolve_address(&bob_hex).await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id }) => {
            assert_eq!(actor_id, bob_actor, "decoded the 32-byte key from the hex");
            assert_eq!(handle, bob_hex, "the typed hex stands in as the handle");
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

/// A well-formed 64-hex actor id whose actor has *no* key packages on the nest is
/// `NotFound` — `count == 0` means unreachable (no package to add them to a group
/// with), so the manager's resolution chain falls through to other backends.
#[tokio::test]
async fn resolve_address_actor_id_hex_no_keypackages_not_found() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_hex = hex::encode(bob.identity_actor_id().0);

    let nest = Arc::new(MockNest::default()); // empty: no key packages for anyone
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    assert_eq!(
        backend.resolve_address(&bob_hex).await,
        ResolveResult::NotFound
    );
}

/// A non-actor-id string that resolves to no Fauna handle on this nest is
/// `NotFound`, so the manager tries other rails (an Email-shaped string resolves
/// on the SMTP backend, etc.). With an empty `MockNest` (no registered handles)
/// every shape — email-like, short-hex, junk — falls through after the
/// `actor_by_handle` lookup misses.
#[tokio::test]
async fn resolve_address_non_actor_id_string_not_found() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    assert_eq!(
        backend.resolve_address("alice@example.com").await,
        ResolveResult::NotFound,
        "no Fauna handle registered → falls through to the SMTP rail"
    );
    assert_eq!(
        backend.resolve_address("deadbeef").await,
        ResolveResult::NotFound,
        "a bare handle with no registered actor"
    );
    assert_eq!(
        backend.resolve_address("nothex".repeat(11).as_str()).await,
        ResolveResult::NotFound,
        "66 chars but not a registered handle"
    );
}

// Form 2 — handle→actor lookup (`fauna.actor.by_handle`). A typed *handle*
// (`alice`, `alice@nest.test`) promotes to `TypedAddress::Fauna` when it
// resolves to a reachable actor on the logged-in nest, lifting `address.rs`'s
// "Cannot produce Fauna" limitation for resolvable handles
// (`docs/goal/ui/conversations.md` § Where logic lives → "MLS key lookup …
// Fauna→Mastodon→Email disambiguation").

/// A bare handle (no domain) registered on this nest, whose actor has a key
/// package, resolves to `Fauna` with the canonical `localpart@domain` display.
#[tokio::test]
async fn resolve_address_bare_handle_resolves_to_fauna() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.seed_handle("bob", &bob_hex, "nest.test");
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    match backend.resolve_address("bob").await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id }) => {
            assert_eq!(actor_id, bob_actor, "handle resolved to bob's actor key");
            assert_eq!(
                handle, "bob@nest.test",
                "canonical localpart@domain display"
            );
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

/// A `localpart@domain` whose domain matches this nest's handle domain resolves
/// to `Fauna` — the FaunaMls probe (tried first by the manager) wins over the
/// Email fallback for a registered Fauna handle.
#[tokio::test]
async fn resolve_address_handle_at_local_domain_resolves_to_fauna() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.seed_handle("bob", &bob_hex, "nest.test");
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    match backend.resolve_address("bob@nest.test").await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id }) => {
            assert_eq!(actor_id, bob_actor);
            assert_eq!(handle, "bob@nest.test");
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

/// A `localpart@domain` whose localpart IS a registered local handle but whose
/// domain is *not* this nest's handle domain is NOT promoted to Fauna — it's a
/// cross-nest handle (deferred) or an email of the same localpart, so the probe
/// declines (`NotFound`) and the manager's chain falls through to the SMTP rail.
/// This is the disambiguation guard: `bob@example.com` must not hijack the local
/// `bob`.
#[tokio::test]
async fn resolve_address_handle_at_foreign_domain_not_promoted() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_hex = hex::encode(bob.identity_actor_id().0);

    let nest = Arc::new(MockNest::default());
    nest.seed_handle("bob", &bob_hex, "nest.test");
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    assert_eq!(
        backend.resolve_address("bob@example.com").await,
        ResolveResult::NotFound,
        "foreign domain → falls through to the SMTP rail, not Fauna"
    );
}

// ── Spec Y2 slice 3: cross-nest (federated) handle resolution ──────────
//
// A typed `localpart@domain` whose domain is NOT this nest's resolves directly
// against that peer nest via the seam's `actor_by_handle_remote`, with
// reachability taken from the reply's `addressable` boolean (a foreign client
// cannot run a `keypackage.count` probe — `federation.md` § Key packages). The
// data plane (key-package fetch / Welcome) then routes through the home-nest
// federation relay via `peer_domain` (→ the request's `nest_url`).

/// A foreign handle whose peer-nest actor is `addressable` promotes to `Fauna`
/// with the canonical `localpart@foreign-domain` display.
#[tokio::test]
async fn resolve_address_foreign_handle_resolves_to_fauna() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    let nest = Arc::new(MockNest::default());
    // Bob lives on the foreign nest and is addressable (≥1 usable key package).
    nest.seed_remote_handle("foreign.test", "bob", &bob_hex, true);

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id }) => {
            assert_eq!(
                actor_id, bob_actor,
                "resolved bob's actor on the foreign nest"
            );
            assert_eq!(handle, "bob@foreign.test", "canonical cross-nest display");
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

// ── The dial names the peer (`federation.md` § Peer-auth model →
// *Discovery-failure semantics*, ratified 2026-09-22). A peer nest answers
// `by_handle` with an actor id AND a domain it names for itself; only the
// first is what the client asked for, and only the DIAL is bound to the nest
// that answered (authenticated TLS). So the canonical handle is always built
// from the dialed domain, and the reply's `echoed_domain` is never read.

/// **Red-verify** — a nest reached at `attacker.test` echoes `trusted.test`.
/// Before the fix the echo won, and the thread stored — and all 7 apps
/// rendered — a participant whose handle read `bob@trusted.test` while the only
/// nest that ever answered was the attacker's. The dial must win.
#[tokio::test]
async fn resolve_address_foreign_echoed_domain_never_overrides_the_dial() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let mallory_hex = hex::encode([7u8; 32]);

    let nest = Arc::new(MockNest::default());
    // The attacker's nest answers for `bob`, with an actor id of its choosing…
    nest.seed_remote_handle("attacker.test", "bob", &mallory_hex, true);
    // …and names a domain it was never reached at.
    nest.set_remote_echo_domain("trusted.test");

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    match backend.resolve_address("bob@attacker.test").await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, .. }) => {
            assert_eq!(
                handle, "bob@attacker.test",
                "the canonical handle names the domain we DIALED — an echoed \
                 `trusted.test` must never reach the stored participant"
            );
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

/// The same rule read from the legitimate side. This hop sends no `domain`
/// qualifier, so a multi-domain peer reached at a **secondary** domain echoes
/// its *primary* identity domain (`mail-multidomain.md` § Resolution and login
/// report the live identity domain) — a perfectly honest reply that differs
/// from the dial. Taking the dial gives the user back the domain they typed;
/// the old echo-wins code silently rewrote `bob@domain2.test` to
/// `bob@primary.test`. This is why the rule is "the dial wins", not "reject a
/// reply that disagrees": rejecting would break this peer.
#[tokio::test]
async fn resolve_address_foreign_multidomain_peer_keeps_the_typed_domain() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_hex = hex::encode([3u8; 32]);

    let nest = Arc::new(MockNest::default());
    nest.seed_remote_handle("domain2.test", "bob", &bob_hex, true);
    nest.set_remote_echo_domain("primary.test");

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    match backend.resolve_address("bob@domain2.test").await {
        ResolveResult::Resolved(TypedAddress::Fauna { handle, actor_id }) => {
            assert_eq!(
                handle, "bob@domain2.test",
                "the user typed `domain2.test` and that is what the chip shows"
            );
            assert_eq!(hex::encode(actor_id.0), bob_hex, "the peer's actor id");
        }
        other => panic!("expected Resolved(Fauna), got {other:?}"),
    }
}

/// The flow's second half, end to end: resolving `bob@peer.test` against a nest
/// that echoes `elsewhere.test` → the participant stored on the thread →
/// `peer_domain_for` deriving the data-plane route from that handle →
/// `bootstrap_group`'s `keypackage_fetch`. The key package must be fetched from
/// the TLS-verified domain we dialed, never from the echoed string — otherwise
/// the peer chooses which nest the client trusts for its key material.
#[tokio::test]
async fn bootstrap_fetches_the_key_package_from_the_dialed_domain() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.seed_remote_handle("peer.test", "bob", &bob_hex, true);
    nest.set_remote_echo_domain("elsewhere.test");
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice@home.test", alice_actor);

    let ResolveResult::Resolved(addr) = backend.resolve_address("bob@peer.test").await else {
        panic!("bob@peer.test resolves");
    };

    let thread = fauna_mls_thread(ThreadId("t-dial".into()), vec![addr]);
    let compose = ComposeState {
        body_draft: "ping".into(),
        ..Default::default()
    };
    backend
        .send(&thread, &compose, &[])
        .await
        .expect("bootstrap+send ok");

    assert_eq!(
        nest.keypackage_fetch_domains(),
        vec![Some("peer.test".to_string())],
        "the data plane routes to the dialed domain, not the echoed one"
    );
}

/// A foreign handle that resolves but is **not addressable** (no usable key
/// package on the peer nest) is `NotFound`, so the chain falls through.
#[tokio::test]
async fn resolve_address_foreign_handle_not_addressable_not_found() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol_hex = hex::encode([9u8; 32]);

    let nest = Arc::new(MockNest::default());
    nest.seed_remote_handle("foreign.test", "carol", &carol_hex, false);

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    assert_eq!(
        backend.resolve_address("carol@foreign.test").await,
        ResolveResult::NotFound,
        "known-but-unaddressable foreign actor falls through"
    );
}

/// An unknown handle on a foreign domain is `NotFound` (the peer `by_handle`
/// misses), falling through to the SMTP rail.
#[tokio::test]
async fn resolve_address_foreign_handle_unknown_not_found() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    assert_eq!(
        backend.resolve_address("nobody@foreign.test").await,
        ResolveResult::NotFound,
    );
}

// ── The paint gate (`contacts.md` § The private overlay → *The paint gate*,
// ruled 2026-09-27): a nickname paints only on a
// proven identity. The dial binds a cross-nest participant's HANDLE to the
// domain the user typed and leaves the ACTOR ID unbound (`federation.md`
// § Peer-auth model → *What a peer's answer may and may not claim*), so a nest
// at the dialed domain may answer `by_handle` with the actor id of someone the
// viewer has nicknamed. The one cryptographic bind on an actor id is a verified
// leaf of an MLS group (`conversations.md` MLS-2), so a member chip paints the
// nickname only once that id is a leaf of the thread's bound group.

/// Load the viewer's own nickname `nick` on `person` into `manager`'s overlay
/// projection (no fold seam — the store is not under test here).
fn nickname_on(manager: &ConversationsManager, person: &ActorId, nick: &str) {
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    let generation = manager.register_contact_overlays(None);
    let overlay = ContactOverlay {
        nickname: Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(nick.into()),
        },
        ..Default::default()
    };
    assert!(manager.apply_contact_overlays(
        generation,
        [(person.to_hex(), overlay)].into_iter().collect()
    ));
}

/// The attack, traced end to end through the manager: the viewer has nicknamed
/// `mum` ("Mum"); a nest serving `attacker.test` answers `bob` with mum's actor
/// id; the resolved participant reaches a thread. Before the fix the member chip
/// read "Mum" the moment the thread existed — nothing had proven that the key
/// behind `bob@attacker.test` is mum's, and the group about to be bootstrapped
/// would fetch its key package from the attacker. The chip must keep the public
/// label until mum's key is a verified leaf of the thread's group.
///
/// Then the honest half of the same flow: the peer nest serves mum's GENUINE key
/// package, the group bootstraps with mum's key as a verified leaf (MLS-2), and
/// the nickname paints — mum really is in the conversation.
#[tokio::test]
async fn a_nickname_paints_on_no_member_chip_until_the_actor_is_a_verified_leaf() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let mum = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let mum_actor = mum.identity_actor_id();
    let mum_hex = hex::encode(mum_actor.0);

    let nest = Arc::new(MockNest::default());
    // The attacker's nest answers `bob` with the nicknamed person's actor id.
    nest.seed_remote_handle("attacker.test", "bob", &mum_hex, true);

    let manager = ConversationsManager::new();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice@home.test",
        alice_actor,
    ));
    manager.register_backend(backend.clone());
    nickname_on(&manager, &mum_actor, "Mum");

    let ResolveResult::Resolved(addr) = backend.resolve_address("bob@attacker.test").await else {
        panic!("bob@attacker.test resolves");
    };
    assert_eq!(addr.person_actor_id(), Some(mum_actor), "the peer's claim");
    let thread_id = manager.create_mls_group(vec![addr]);

    let detail = manager.thread_detail(thread_id.clone()).expect("thread");
    assert_eq!(
        detail.participant_displays,
        vec!["bob@attacker.test".to_string()],
        "no group has proven this actor id yet: the chip keeps the public label, \
         never the nickname the peer's answer would borrow"
    );

    // The honest half: the dialed nest serves mum's genuine key package, so
    // the bootstrapped group seats mum's own key as a verified leaf.
    nest.seed_keypackage(
        &mum_hex,
        mum.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    manager.select_thread(thread_id.clone());
    manager.set_compose_body(thread_id.clone(), "hello".into());
    manager
        .send(thread_id.clone())
        .await
        .expect("bootstrap + send");

    let detail = manager.thread_detail(thread_id).expect("thread");
    assert_eq!(
        detail.participant_displays,
        vec!["Mum".to_string()],
        "mum's key is a verified leaf of the thread's group: the nickname paints"
    );
}

/// The ratified residual, reached: the dialed nest serves a key package for a
/// key of ITS OWN choosing in place of the requested actor's (`federation.md`
/// § Peer-auth model: the delivery service is trusted for the requested actor's
/// key material, and the dial rule only narrows which service that is). The
/// group then seats the attacker's key while the thread's participant row still
/// carries mum's actor id. The chip must not read "Mum": the leaf that was
/// admitted is not hers.
#[tokio::test]
async fn a_substituted_key_package_never_earns_the_nickname() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let mum_actor = ActorId([5u8; 32]);
    let mum_hex = hex::encode(mum_actor.0);
    let mallory = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();

    let nest = Arc::new(MockNest::default());
    nest.seed_remote_handle("attacker.test", "bob", &mum_hex, true);
    // Asked for mum's key package, the attacker's nest serves mallory's — an
    // honest package for mallory's own key, which MLS-2 admits as mallory.
    nest.seed_keypackage(
        &mum_hex,
        mallory.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let manager = ConversationsManager::new();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice@home.test",
        alice_actor,
    ));
    manager.register_backend(backend.clone());
    nickname_on(&manager, &mum_actor, "Mum");

    let ResolveResult::Resolved(addr) = backend.resolve_address("bob@attacker.test").await else {
        panic!("bob@attacker.test resolves");
    };
    let thread_id = manager.create_mls_group(vec![addr]);
    manager.select_thread(thread_id.clone());
    manager.set_compose_body(thread_id.clone(), "hello".into());
    manager
        .send(thread_id.clone())
        .await
        .expect("bootstrap + send");

    let roster = backend
        .authoritative_roster(&thread_id)
        .expect("the thread is bound to a group");
    assert!(
        roster.contains(&mallory.identity_actor_id()) && !roster.contains(&mum_actor),
        "the group seated the substituted key, not mum's"
    );
    let detail = manager.thread_detail(thread_id).expect("thread");
    assert_eq!(
        detail.participant_displays,
        vec!["bob@attacker.test".to_string()],
        "the admitted leaf is not mum's key: the chip keeps the public label"
    );
}

/// The secondary effect under the same gate: `handle_for_person` backfills a
/// nameless seat from any thread carrying the same actor id. A seat whose
/// actor id no group has proven lends nothing — otherwise the forged
/// `(bob@attacker.test, mum)` pair puts the dialed handle on mum's genuinely
/// nameless seat one thread over. Once mum's key is a verified leaf of that
/// thread's group, the handle the dialed domain routes to her key is hers to
/// lend.
#[tokio::test]
async fn an_unproven_seat_lends_no_handle_to_a_nameless_seat_elsewhere() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let mum = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let mum_actor = mum.identity_actor_id();
    let mum_hex = hex::encode(mum_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.seed_remote_handle("attacker.test", "bob", &mum_hex, true);

    let manager = ConversationsManager::new();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice@home.test",
        alice_actor,
    ));
    manager.register_backend(backend.clone());

    let ResolveResult::Resolved(addr) = backend.resolve_address("bob@attacker.test").await else {
        panic!("bob@attacker.test resolves");
    };
    let thread_id = manager.create_mls_group(vec![addr]);
    assert_eq!(
        manager.handle_for_person(&mum_actor),
        None,
        "a seat no group has proven lends its handle to nobody"
    );
    assert_eq!(
        manager.seat_address_for(mum_actor).person_handle(),
        None,
        "so a nameless seat elsewhere stays nameless (its short id renders)"
    );

    nest.seed_keypackage(
        &mum_hex,
        mum.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    manager.select_thread(thread_id.clone());
    manager.set_compose_body(thread_id.clone(), "hello".into());
    manager.send(thread_id).await.expect("bootstrap + send");
    assert_eq!(
        manager.handle_for_person(&mum_actor).as_deref(),
        Some("bob@attacker.test"),
        "a verified leaf's seat lends the handle the dialed domain routes to that key"
    );
}

// ── Discovery-failure semantics (`federation.md` § Peer-auth model, ratified
// 2026-08-29): a foreign nest that does not ANSWER is told apart from one that
// answers "no such actor" by positive evidence the client holds identically on
// every arm — never by the transport-error kind, which the browser cannot see.

fn transport_fault() -> ConvRpcError {
    ConvRpcError::transient("connect https://foreign.test: connection refused")
}

fn fauna_participant(handle: &str, byte: u8) -> TypedAddress {
    TypedAddress::Fauna {
        handle: handle.to_string(),
        actor_id: ActorId([byte; 32]),
    }
}

/// First contact with a domain nothing vouches for, and no nest answers: the
/// address falls through to email exactly like a domain with no nest at all.
/// (`bob@example.com` dials `https://example.com` and fails the same way.)
///
/// The carve-out rests on an absence this client **established**, so the
/// account's conversations must be loaded for the question to be answerable at
/// all — see `resolve_address_foreign_non_answer_before_restore_never_downgrades_to_email`
/// for the window before that.
#[tokio::test]
async fn resolve_address_foreign_transport_fault_unknown_domain_falls_through() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    nest.fail_remote_lookups(Some(transport_fault()));
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);
    backend.mark_conversations_loaded();

    assert_eq!(
        backend.resolve_address("bob@foreign.test").await,
        ResolveResult::NotFound,
        "no evidence of a Fauna nest at the domain → email fallthrough"
    );
}

/// A domain this account already converses with over Fauna (a
/// `TypedAddress::Fauna` participant carries it) is a KNOWN Fauna domain: when
/// its nest does not answer, the resolve is an `Error` — never a silent
/// downgrade of the message to plaintext email.
#[tokio::test]
async fn resolve_address_foreign_transport_fault_known_by_thread_is_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    nest.fail_remote_lookups(Some(transport_fault()));
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    // The manager hands every backend the participants of every thread before
    // a probe (`ConversationsManager::probe_address`); an existing Fauna thread
    // with carol vouches for `Foreign.TEST` (domain match is case-insensitive).
    // The restore has landed, so the tail assertion below is a real absence
    // rather than an unread store.
    backend.mark_conversations_loaded();
    backend.observe_participants(&[
        fauna_participant("carol@Foreign.TEST", 7),
        TypedAddress::Email {
            email_address: "dave@plain.test".into(),
        },
    ]);

    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Error(_) => {}
        other => panic!("known Fauna domain unreachable must be Error, got {other:?}"),
    }
    // An Email participant vouches for nothing: `plain.test` still falls through.
    assert_eq!(
        backend.resolve_address("erin@plain.test").await,
        ResolveResult::NotFound,
    );
}

/// A nest that answered earlier this session — even "no such actor" — proved
/// a Fauna nest lives at the domain; a later non-answer is an `Error`.
#[tokio::test]
async fn resolve_address_foreign_transport_fault_known_by_earlier_answer_is_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice@home.test", alice_actor);

    // The peer answers `not_found` (seam `Ok(None)`): email fallthrough, and
    // the domain is now known to host a nest.
    assert_eq!(
        backend.resolve_address("nobody@foreign.test").await,
        ResolveResult::NotFound,
    );
    nest.fail_remote_lookups(Some(transport_fault()));
    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Error(_) => {}
        other => panic!("a domain that answered before must be Error now, got {other:?}"),
    }
}

/// **The cold-start window** — `federation.md` § Peer-auth model →
/// *Discovery-failure semantics*, clause (b) and its closing sentence.
///
/// The ruling's carve-out ends "the moment any Fauna thread with that domain
/// exists", and clause (b) scopes that evidence to **this account's**
/// conversations. What the rail can actually see is the participant harvest off
/// the in-memory `ThreadStore` — and that store is empty until the replica
/// restore (`fauna_client_mls_sync::orchestration::restore_and_wire`) has
/// fetched this account's history *over the network* from the home nest. So
/// between launch and restore, an account that has conversed with `peer.test`
/// over Fauna for months holds no evidence of it in this process.
///
/// An un-hydrated store must therefore never be read as "this account has no
/// Fauna relationship with the domain": only a **loaded** absence justifies the
/// email carve-out. Before this, a peer that did not answer inside that window
/// fell through to the SMTP rail — an email chip, and a send in the clear, for
/// a peer MLS would have encrypted.
#[tokio::test]
async fn resolve_address_foreign_non_answer_before_restore_never_downgrades_to_email() {
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
    }
    #[async_trait]
    impl OutboundMailSink for RecordingSink {
        async fn submit(
            &self,
            recipients: Vec<String>,
            raw_rfc5322: Vec<u8>,
        ) -> Result<(), String> {
            self.sent.lock().unwrap().push((recipients, raw_rfc5322));
            Ok(())
        }
    }

    // A cold start, exactly as a leg assembles one: both rails registered, and
    // NOTHING restored yet — no `restore_channel_slice`, so the `ThreadStore`
    // the harvest reads is empty even though the account's conversations exist
    // on the home nest.
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    let session = ConversationsSession::from_parts(
        engine,
        nest.clone(),
        "alice@home.test".into(),
        actor,
        None,
    );
    session.register_smtp(Arc::new(RecordingSink::default()));

    // The peer's nest does not answer (DNS/connect/TLS/WS/timeout — the kind is
    // deliberately not consulted).
    nest.fail_remote_lookups(Some(transport_fault()));

    let m = session.manager();
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@peer.test".into());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(
        picker.resolve_state,
        ResolveState::Error,
        "a non-answer with the account's evidence still unread is a failed lookup, \
         never a licence to downgrade to plaintext email"
    );
    assert_eq!(picker.resolved, None, "no rail vouched for an address");
    assert!(
        !m.accept_current_recipient_chip(),
        "an errored resolve commits no chip — not even an email one by shape"
    );

    // ── The control, and the reason this is a window and not a new refusal.
    // Same session, same unreachable peer — but the restore has now landed, so
    // the empty harvest is a real absence and the ruling's first-contact arm
    // opens exactly as before: an unadvertised, unreachable domain is email.
    session.backend().mark_conversations_loaded();
    m.set_new_thread_recipient_input("bob@peer.test".into());
    m.resolve_recipient().await;

    let picker = m
        .snapshot()
        .new_thread_compose
        .unwrap()
        .recipient_picker
        .unwrap();
    assert_eq!(
        picker.resolve_state,
        ResolveState::Resolved,
        "with the account's evidence read and empty, first contact is email by ruling"
    );
    assert!(
        matches!(picker.resolved, Some(TypedAddress::Email { .. })),
        "the SMTP rail claims it once the absence is established; got {:?}",
        picker.resolved
    );
}

/// A nest that answered with a definite refusal other than `not_found`
/// (`domain_not_local`, `handle.invalid`, …) is a nest answering "not a Fauna
/// recipient here": the chain falls through, and the refusal vouches for
/// nothing (the nest disowned the domain).
#[tokio::test]
async fn resolve_address_foreign_rejected_is_not_found_and_vouches_nothing() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice@home.test", alice_actor);
    // The claim under test is about what the REFUSAL vouched for, so the
    // account's evidence must be loaded — otherwise the second probe falls
    // through on the cold-start rule and proves nothing about `learn_domain`.
    backend.mark_conversations_loaded();

    nest.fail_remote_lookups(Some(ConvRpcError::Rejected {
        message: "domain not local".into(),
    }));
    assert_eq!(
        backend.resolve_address("bob@foreign.test").await,
        ResolveResult::NotFound,
    );
    nest.fail_remote_lookups(Some(transport_fault()));
    assert_eq!(
        backend.resolve_address("bob@foreign.test").await,
        ResolveResult::NotFound,
        "a refusal is not evidence of a nest serving this domain"
    );
}

/// A version-incompatible peer (`NeedsUpdate`) IS a Fauna nest we cannot talk
/// to: `Error`, not email.
#[tokio::test]
async fn resolve_address_foreign_needs_update_is_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    nest.fail_remote_lookups(Some(ConvRpcError::NeedsUpdate {
        message: "update your app".into(),
    }));
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Error(_) => {}
        other => panic!("an outdated-version refusal must be Error, got {other:?}"),
    }
}

/// The home nest not answering does not stop a typed FOREIGN domain from being
/// probed directly (the peer may well be up): the same-nest hop's transport
/// fault is classified against the local actor's own domain, and a different
/// typed domain routes on to `resolve_foreign`.
#[tokio::test]
async fn resolve_address_home_nest_down_typed_foreign_domain_still_probes_peer() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.fail_home_lookups(Some(transport_fault()));
    nest.seed_remote_handle("foreign.test", "bob", &hex::encode(bob_actor.0), true);
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Resolved(TypedAddress::Fauna { actor_id, .. }) => {
            assert_eq!(actor_id, bob_actor);
        }
        other => panic!("the peer was reachable and must resolve, got {other:?}"),
    }
}

/// The home domain is always a known Fauna domain: a bare handle, or a handle
/// typed at the local actor's own domain, is an `Error` while the home nest
/// does not answer — never email.
#[tokio::test]
async fn resolve_address_home_nest_down_own_domain_or_bare_handle_is_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    nest.fail_home_lookups(Some(transport_fault()));
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    for raw in ["bob", "bob@home.test", "bob@HOME.test"] {
        match backend.resolve_address(raw).await {
            ResolveResult::Error(_) => {}
            other => panic!("{raw}: home nest down must be Error, got {other:?}"),
        }
    }
}

/// An address at the local actor's OWN domain that the home nest answers "no
/// such handle" for is not a Fauna handle — the nest that owns the domain has
/// answered for it (`foreign-handle-resolution.md` § Peer-auth model: the home
/// domain is always known). It declines (`NotFound`) so the chain reaches the
/// SMTP rail: an alias or a mailing list at the user's own mail domain is
/// addressable as mail (`mail-mass-mailing.md` § Composing a list message).
/// It is never re-probed as a foreign domain, whose failed hop on a known
/// domain would read as a terminal `Error`.
#[tokio::test]
async fn resolve_address_own_domain_non_handle_declines_to_email() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    *nest.remote_lookup_fault.lock().unwrap() = Some(transport_fault());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice@home.test", alice_actor);

    for raw in ["news@home.test", "news@HOME.test"] {
        assert_eq!(
            backend.resolve_address(raw).await,
            ResolveResult::NotFound,
            "{raw}: the home nest disowned it, so it is mail"
        );
    }
}

/// Before identity resolution the local actor's domain is unknown, so a typed
/// domain cannot be classified as foreign; the conservative answer while the
/// home nest does not answer is `Error`, not an email guess.
#[tokio::test]
async fn resolve_address_home_nest_down_self_domain_unknown_is_error() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    nest.fail_home_lookups(Some(transport_fault()));
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    match backend.resolve_address("bob@foreign.test").await {
        ResolveResult::Error(_) => {}
        other => panic!("self domain unknown + home down must be Error, got {other:?}"),
    }
}

/// The data plane routes a **foreign** participant's key-package fetch + Welcome
/// through the relay: the peer's handle domain (≠ ours) surfaces as `peer_domain`
/// on the seam calls (which the seam impl maps to the request's `nest_url`).
#[tokio::test]
async fn bootstrap_group_routes_foreign_participant_via_peer_domain() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    // Bob's real key package is fetchable (the MockNest stores it under bob's
    // actor id regardless of route, so group bootstrap succeeds).
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice@home.test", alice_actor);

    // New 1:1 thread with Bob, whose canonical handle names the foreign nest.
    let thread = fauna_mls_thread(
        ThreadId("t-x".into()),
        vec![fauna_addr("bob@foreign.test", bob_actor)],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hi".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("bootstrap+send ok");

    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1);
    assert_eq!(
        welcomes[0].peer_domain.as_deref(),
        Some("foreign.test"),
        "foreign participant's Welcome routed via peer_domain (→ nest_url relay)"
    );
}

/// A same-nest participant carries `peer_domain == None` (no relay) — the peer's
/// handle domain equals ours.
#[tokio::test]
async fn bootstrap_group_same_nest_participant_has_no_peer_domain() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let bob_hex = hex::encode(bob_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &bob_hex,
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest.clone(), "alice@home.test", alice_actor);

    // Bob on the SAME nest (home.test) → no relay.
    let thread = fauna_mls_thread(
        ThreadId("t-y".into()),
        vec![fauna_addr("bob@home.test", bob_actor)],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hi".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("bootstrap+send ok");

    let welcomes = nest.welcomes();
    assert_eq!(welcomes.len(), 1);
    assert_eq!(welcomes[0].peer_domain, None, "same-nest peer → no relay");
}

/// A registered handle whose actor has *no* key package is unreachable (nothing
/// to add them to a group with), so resolution is `NotFound` — same invariant as
/// the actor-id form (`Resolved(Fauna)` ⇒ startable group).
#[tokio::test]
async fn resolve_address_handle_resolved_but_no_keypackage_not_found() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_hex = hex::encode(bob.identity_actor_id().0);

    let nest = Arc::new(MockNest::default());
    nest.seed_handle("bob", &bob_hex, "nest.test"); // registered but no key packages

    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    assert_eq!(
        backend.resolve_address("bob").await,
        ResolveResult::NotFound,
        "registered but unreachable (no key package)"
    );
}

/// Shapes that belong to other rails are declined by the FaunaMls handle probe
/// without a lookup — Mastodon (`@user@instance`), Nostr (`npub1…`), Bluesky
/// (`did:…`) — so the manager's chain routes them to those backends.
#[tokio::test]
async fn resolve_address_other_rail_shapes_not_probed_as_handles() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    // Even with a `user` handle registered, the Mastodon `@user@instance` shape
    // must not resolve it.
    nest.seed_handle("user", &hex::encode([7u8; 32]), "instance.example");
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice, nest, "alice", alice_actor);

    for shape in [
        "@user@instance.example",
        "npub1xyz",
        "did:plc:abc123",
        "   ",
    ] {
        assert_eq!(
            backend.resolve_address(shape).await,
            ResolveResult::NotFound,
            "{shape} belongs to another rail / is junk"
        );
    }
}

// ── Track E: key-package replenishment (login glue) ────────────────────

/// `ensure_keypackages` tops the local actor's nest queue up to `target` and is
/// idempotent thereafter — the login-time replenish so peers can fetch a package
/// to add us to a group. An empty queue uploads exactly `target`; a second call
/// at `target` is a no-op; a partial queue uploads only the shortfall.
#[tokio::test]
async fn ensure_keypackages_tops_up_to_target_and_is_idempotent() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let alice_hex = hex::encode(alice_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.set_upload_actor(&alice_hex);
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    // Empty queue → uploads exactly `target`.
    let uploaded = backend.ensure_keypackages(5).await.expect("ensure ok");
    assert_eq!(uploaded, 5, "empty queue tops up to target");
    assert_eq!(nest.keypackage_count_for(&alice_hex), 5);

    // Already at target → no-op, no further upload.
    let again = backend.ensure_keypackages(5).await.expect("ensure ok");
    assert_eq!(again, 0, "at target is a no-op");
    assert_eq!(nest.keypackage_count_for(&alice_hex), 5);
}

/// Below target → `ensure_keypackages` uploads only the shortfall.
#[tokio::test]
async fn ensure_keypackages_uploads_only_the_shortfall() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let alice_hex = hex::encode(alice_actor.0);

    let nest = Arc::new(MockNest::default());
    // Two packages already published; target is five.
    nest.seed_keypackage(
        &alice_hex,
        alice.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    nest.seed_keypackage(
        &alice_hex,
        alice.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    nest.set_upload_actor(&alice_hex);

    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let uploaded = backend.ensure_keypackages(5).await.expect("ensure ok");
    assert_eq!(uploaded, 3, "uploads only the shortfall (5 - 2)");
    assert_eq!(nest.keypackage_count_for(&alice_hex), 5);
}

/// `ensure_last_resort_keypackage` publishes exactly ONE reusable last-resort
/// key package (the mandatory onboarding publication, Spec Y2 —
/// `docs/goal/architecture/federation.md` § Key packages). Calling it twice is
/// idempotent: the nest keeps a single last-resort row per actor, so a re-publish
/// on the next login replaces rather than accumulates. The last-resort upload is
/// also distinct from the one-time pool (`ensure_keypackages`) — it never lands
/// in the consumable queue.
#[tokio::test]
async fn ensure_last_resort_keypackage_publishes_one_reusable_package() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let alice_hex = hex::encode(alice_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.set_upload_actor(&alice_hex);
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    // First publication → exactly one last-resort KP, nothing in the one-time pool.
    backend
        .ensure_last_resort_keypackage()
        .await
        .expect("publish ok");
    assert_eq!(
        nest.last_resort_count_for(&alice_hex),
        1,
        "exactly one last-resort KP published"
    );
    assert_eq!(
        nest.keypackage_count_for(&alice_hex),
        0,
        "last-resort upload does not touch the consumable one-time pool"
    );

    // Second publication (next login) → still exactly one (idempotent replace).
    backend
        .ensure_last_resort_keypackage()
        .await
        .expect("re-publish ok");
    assert_eq!(
        nest.last_resort_count_for(&alice_hex),
        1,
        "re-publishing does not accumulate a second last-resort KP"
    );

    // The one-time top-up still works alongside (and stays separate).
    let uploaded = backend.ensure_keypackages(3).await.expect("ensure ok");
    assert_eq!(uploaded, 3);
    assert_eq!(nest.keypackage_count_for(&alice_hex), 3);
    assert_eq!(
        nest.last_resort_count_for(&alice_hex),
        1,
        "the one-time top-up leaves the single last-resort KP untouched"
    );
}

/// Track E (FFI export): the manager wrappers `ensure_keypackages` /
/// `ensure_last_resort_keypackage` route through `dyn RailBackend` to the
/// registered FaunaMls backend — the path every FFI client drives at login
/// (clients hold only the manager, never the backend). Proves the manager
/// reaches the MLS key-package machinery without itself touching `fauna-mls`.
#[tokio::test]
async fn manager_ensure_keypackages_routes_to_registered_fauna_mls_backend() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let alice_hex = hex::encode(alice_actor.0);

    let nest = Arc::new(MockNest::default());
    nest.set_upload_actor(&alice_hex);
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));

    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());

    // One-time pool top-up via the manager wrapper.
    let uploaded = manager.ensure_keypackages(5).await.expect("ensure ok");
    assert_eq!(
        uploaded, 5,
        "manager routes the top-up to the FaunaMls backend"
    );
    assert_eq!(nest.keypackage_count_for(&alice_hex), 5);

    // Last-resort publication via the manager wrapper.
    manager
        .ensure_last_resort_keypackage()
        .await
        .expect("publish ok");
    assert_eq!(nest.last_resort_count_for(&alice_hex), 1);
}

/// With no FaunaMls backend registered (e.g. a client whose only rail is mail,
/// or a test fixture), the manager wrappers are graceful no-ops — `Ok(0)` /
/// `Ok(())` — so login glue can call them unconditionally on every app.
#[tokio::test]
async fn manager_ensure_keypackages_is_a_noop_without_a_fauna_mls_backend() {
    let manager = ConversationsManager::new();
    assert_eq!(manager.ensure_keypackages(5).await.expect("ok"), 0);
    manager.ensure_last_resort_keypackage().await.expect("ok");
}

/// Red-first durability pin: a key package minted
/// through the manager's [`ConversationsManager::ensure_keypackages`] entry
/// point — the surface EVERY native replenish path now routes through (login,
/// linux auto/settings, android settings; web/windows already did) — survives a
/// later provider-storage swap, because the mint `notify()`s the snapshot
/// observer so the replica autosave captures the fresh private init keys BEFORE
/// a mid-session `resync_provider` / relaunch `restore_into` swaps the engine's
/// KV.
///
/// The two arms are the same before/after contrast as fauna-mls
/// `key_package_minted_before_provider_swap_loses_its_init_key`, one layer up at
/// the replenish entry point:
/// - **Arm 1 (the bug)** — a raw `engine.generate_key_packages_bytes` mint that
///   skips the observer tick (the pre-fix native direct-mint path: linux
///   `MlsManager::generate_key_packages` + `publish_key_packages_real`, android's
///   throwaway-engine `mls_generate_key_packages`) leaves the captured replica
///   stale, so the swap wipes the init key and the peer's Welcome can never join.
/// - **Arm 2 (the fix)** — the mint via `ensure_keypackages` notifies, so the
///   captured replica carries the init keys and the Welcome joins after the swap.
///
/// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync.
#[tokio::test]
async fn keypackage_minted_via_ensure_keypackages_survives_a_provider_swap() {
    use fauna_mls::state_replica::ProviderReplica;
    use std::sync::Mutex as StdMutex;

    // Stand-in for the production debounced replica autosave: on every manager
    // `notify()` it snapshots the backend engine's provider into `replica` —
    // exactly what `attach_replica_autosave`'s observer does — so a captured
    // replica reflects the engine state as of the last tick.
    struct AutosaveObserver {
        engine: Arc<MlsEngine>,
        replica: Arc<StdMutex<ProviderReplica>>,
    }
    impl SnapshotObserver for AutosaveObserver {
        fn on_changed(&self) {
            *self.replica.lock().unwrap() = ProviderReplica::from_engine(&self.engine);
        }
    }

    // ── Arm 1 (the bug): a raw engine mint that skips the notify ─────────────
    {
        let alice =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7u8; 32])).unwrap());
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([8u8; 32])).unwrap();

        // The last durably-saved replica is the pre-mint baseline: no observer
        // ticked for this mint, so the autosave never captured it.
        let replica = ProviderReplica::from_engine(&alice);

        // Mint directly on the engine + a peer fetches the raw bytes (the pool).
        let kp_bytes = alice.generate_key_packages_bytes(1).unwrap().remove(0);
        let kp = bob.validate_key_package(&kp_bytes).unwrap();
        let (_ch, welcome) = bob.create_group(&[kp]).unwrap();

        // A later provider swap restores the STALE replica → the init key is gone.
        replica.restore_into_unchecked(&alice).unwrap();
        assert!(
            alice.join_from_welcome(welcome).is_err(),
            "a raw mint that skipped the autosave notify is wiped by the swap",
        );
    }

    // ── Arm 2 (the fix): mint via the durable `ensure_keypackages` entry point ─
    {
        let alice =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7u8; 32])).unwrap());
        let alice_actor = alice.identity_actor_id();
        let alice_hex = hex::encode(alice_actor.0);
        let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([8u8; 32])).unwrap();

        let nest = Arc::new(MockNest::default());
        nest.set_upload_actor(&alice_hex);
        let backend = Arc::new(FaunaMlsBackend::new(
            alice.clone(),
            nest.clone(),
            "alice",
            alice_actor,
        ));
        let manager = ConversationsManager::new();
        manager.register_backend(backend.clone());

        // The autosave observer, seeded with the pre-mint baseline.
        let replica = Arc::new(StdMutex::new(ProviderReplica::from_engine(&alice)));
        manager.add_observer(Arc::new(AutosaveObserver {
            engine: alice.clone(),
            replica: replica.clone(),
        }));

        // Durable entry point: mint + upload + `notify()` → the observer captures
        // a replica that INCLUDES the fresh private init keys (notify is
        // synchronous within the await — see `keypackage_mint_ticks_snapshot_observers`).
        assert_eq!(manager.ensure_keypackages(1).await.unwrap(), 1);

        // A peer fetches the published KP and creates a group.
        let kp_bytes = nest
            .keypackage_fetch(alice_hex.clone(), None)
            .await
            .unwrap()
            .expect("a key package was published to the pool");
        let kp = bob.validate_key_package(&kp_bytes).unwrap();
        let (_ch, welcome) = bob.create_group(&[kp]).unwrap();

        // The mid-session `resync_provider` swap restores the FRESH replica →
        // the init key survived, so the Welcome joins.
        replica
            .lock()
            .unwrap()
            .restore_into_unchecked(&alice)
            .unwrap();
        alice
            .join_from_welcome(welcome)
            .expect("a KP minted via ensure_keypackages survives the provider swap");
    }
}

/// Red-first durability pin: a key-package
/// mint publishes to the nest pool **only after** its fresh private init keys are
/// durable in the cross-device replica. The prior pin
/// (`keypackage_minted_via_ensure_keypackages_survives_a_provider_swap`) modelled
/// the autosave observer as **synchronous and always attached** — exactly the
/// premise that fails in the launch window, where the observer does not exist yet
/// and every replica save no-ops until the cross-device restore lifts the gate.
/// So a mint there would ship a package whose init key the imminent restore
/// deterministically wipes, stranding a peer's group (the web-slice-6 bug class).
///
/// The fix gates the publish on the injected [`ProviderPersist`] flush: it must
/// report the `provider` **durably landed** (`Ok(true)`) or the backend refuses
/// to publish. This test drives that gate through a recording seam:
/// - **launch gate down** (`Ok(false)`) → the mint FAILS and publishes nothing;
/// - **transport failure** (`Err`) → likewise refuses to publish;
/// - **durable** (`Ok(true)`) → publishes, and the flush ran while the pool was
///   still empty (save-**before**-publish ordering);
/// - **no seam** (single-device / no plane) → publishes directly (no replica
///   exists that a swap could restore over the init keys).
///
/// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync, Rule 3.
#[tokio::test]
async fn keypackage_mint_refuses_to_publish_until_the_provider_replica_is_durable() {
    use fauna_conversations::backend::ProviderPersist;
    use std::sync::Mutex as StdMutex;

    #[derive(Clone, Copy)]
    enum SeamResult {
        /// The provider blob durably landed (post-restore, healthy path).
        Wrote,
        /// The save no-op'd — launch gate not yet lifted (the strand window).
        GateDown,
        /// A genuine transport failure mid-flush.
        Err,
    }

    /// A [`ProviderPersist`] stand-in that returns a configured outcome and
    /// records the pool's key-package count for `actor_hex` **at the instant it
    /// runs** — `Some(0)` proves the mint consulted the durable flush BEFORE it
    /// published anything (save-before-publish).
    struct RecordingProviderPersist {
        result: SeamResult,
        seen_pool_count: Arc<StdMutex<Option<u64>>>,
        nest: Arc<MockNest>,
        actor_hex: String,
    }

    #[async_trait]
    impl ProviderPersist for RecordingProviderPersist {
        async fn persist_provider(&self) -> Result<bool, BackendError> {
            *self.seen_pool_count.lock().unwrap() =
                Some(self.nest.keypackage_count_for(&self.actor_hex));
            match self.result {
                SeamResult::Wrote => Ok(true),
                SeamResult::GateDown => Ok(false),
                // `Internal`, matching the real impl
                // (`fauna_client_mls_sync::orchestration::persist_provider`
                // returns `Internal("durable provider flush: …")`): a storage
                // fault is a diagnostic, not a seam's user-facing sentence.
                SeamResult::Err => Err(BackendError::Internal(
                    "simulated launch-time replica flush failure".into(),
                )),
            }
        }
    }

    fn setup() -> (Arc<MlsEngine>, String, Arc<MockNest>, Arc<FaunaMlsBackend>) {
        let alice =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7u8; 32])).unwrap());
        let alice_actor = alice.identity_actor_id();
        let alice_hex = hex::encode(alice_actor.0);
        let nest = Arc::new(MockNest::default());
        nest.set_upload_actor(&alice_hex);
        let backend = Arc::new(FaunaMlsBackend::new(
            alice.clone(),
            nest.clone(),
            "alice",
            alice_actor,
        ));
        (alice, alice_hex, nest, backend)
    }

    fn wire(
        backend: &Arc<FaunaMlsBackend>,
        nest: &Arc<MockNest>,
        actor_hex: &str,
        result: SeamResult,
    ) -> Arc<StdMutex<Option<u64>>> {
        let seen = Arc::new(StdMutex::new(None));
        backend.set_provider_persist(Arc::new(RecordingProviderPersist {
            result,
            seen_pool_count: seen.clone(),
            nest: nest.clone(),
            actor_hex: actor_hex.to_string(),
        }));
        seen
    }

    // ── Launch gate DOWN → refuse to publish, both mint paths ────────────────
    {
        let (_alice, alice_hex, nest, backend) = setup();
        let seen = wire(&backend, &nest, &alice_hex, SeamResult::GateDown);
        backend
            .ensure_keypackages(5)
            .await
            .expect_err("a mint whose init keys are not yet durable must fail, not publish");
        assert_eq!(
            nest.keypackage_count_for(&alice_hex),
            0,
            "no package may reach the pool before its init key is durable",
        );
        assert_eq!(
            *seen.lock().unwrap(),
            Some(0),
            "the durable flush must be consulted before any publish",
        );
        backend
            .ensure_last_resort_keypackage()
            .await
            .expect_err("the last-resort mint must also refuse to publish when not durable");
        assert_eq!(nest.last_resort_count_for(&alice_hex), 0);
    }

    // ── Transport failure mid-flush → refuse to publish ──────────────────────
    {
        let (_alice, alice_hex, nest, backend) = setup();
        wire(&backend, &nest, &alice_hex, SeamResult::Err);
        backend
            .ensure_keypackages(3)
            .await
            .expect_err("a failed durable flush must not publish a doomed package");
        assert_eq!(nest.keypackage_count_for(&alice_hex), 0);
    }

    // ── Durable → publish, save-before-publish ordering ──────────────────────
    {
        let (_alice, alice_hex, nest, backend) = setup();
        let seen = wire(&backend, &nest, &alice_hex, SeamResult::Wrote);
        let minted = backend
            .ensure_keypackages(2)
            .await
            .expect("a durable flush lets the mint publish");
        assert_eq!(minted, 2);
        assert_eq!(nest.keypackage_count_for(&alice_hex), 2);
        assert_eq!(
            *seen.lock().unwrap(),
            Some(0),
            "the provider must be saved BEFORE the package is published",
        );
        backend
            .ensure_last_resort_keypackage()
            .await
            .expect("a durable flush lets the last-resort mint publish");
        assert_eq!(nest.last_resort_count_for(&alice_hex), 1);
    }

    // ── No seam (single-device / no plane) → publish directly (additive) ─────
    {
        let (_alice, alice_hex, nest, backend) = setup();
        let minted = backend
            .ensure_keypackages(2)
            .await
            .expect("no multi-device plane → the direct publish stands");
        assert_eq!(minted, 2);
        assert_eq!(nest.keypackage_count_for(&alice_hex), 2);
    }
}

/// The pre-seam window of the save-before-publish rule: a leg that builds the
/// replica plane declares it at session build
/// ([`FaunaMlsBackend::expect_replica_restore`]), and from then until
/// `restore_and_wire` injects the [`ProviderPersist`] seam a key-package mint —
/// one-time or last-resort — refuses to publish, because the imminent restore
/// swaps the engine's provider and would wipe the fresh init keys while a peer
/// holds the package (the linux replenish-before-restore strand after a
/// succession). The seam's injection lifts the refusal; a restore that fails
/// permanently ([`FaunaMlsBackend::abandon_replica_restore`] — the session
/// stays single-device, no swap is coming) returns to the direct publish.
///
/// `docs/goal/behavior/devices.md` § Cross-device MLS group-state sync, Rule 3.
#[tokio::test]
async fn keypackage_mint_refuses_to_publish_while_the_replica_restore_is_pending() {
    use fauna_conversations::backend::ProviderPersist;

    struct DurableProviderPersist;

    #[async_trait]
    impl ProviderPersist for DurableProviderPersist {
        async fn persist_provider(&self) -> Result<bool, BackendError> {
            Ok(true)
        }
    }

    fn setup() -> (String, Arc<MockNest>, Arc<FaunaMlsBackend>) {
        let alice =
            Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([7u8; 32])).unwrap());
        let alice_actor = alice.identity_actor_id();
        let alice_hex = hex::encode(alice_actor.0);
        let nest = Arc::new(MockNest::default());
        nest.set_upload_actor(&alice_hex);
        let backend = Arc::new(FaunaMlsBackend::new(
            alice,
            nest.clone(),
            "alice",
            alice_actor,
        ));
        (alice_hex, nest, backend)
    }

    // ── Plane expected, seam not yet injected → refuse, both mint paths ──────
    {
        let (alice_hex, nest, backend) = setup();
        backend.expect_replica_restore();
        backend
            .ensure_keypackages(5)
            .await
            .expect_err("a mint before the restore wires the persist seam must fail, not publish");
        assert_eq!(
            nest.keypackage_count_for(&alice_hex),
            0,
            "no package may reach the pool while the restore that would wipe its init key is pending",
        );
        backend
            .ensure_last_resort_keypackage()
            .await
            .expect_err("the last-resort mint must refuse too while the restore is pending");
        assert_eq!(nest.last_resort_count_for(&alice_hex), 0);

        // The restore lands and injects the seam → the same backend now publishes.
        backend.set_provider_persist(Arc::new(DurableProviderPersist));
        assert_eq!(
            backend
                .ensure_keypackages(2)
                .await
                .expect("once the seam is injected the durable mint publishes"),
            2,
        );
        assert_eq!(nest.keypackage_count_for(&alice_hex), 2);
    }

    // ── Restore failed permanently → single-device, direct publish stands ────
    {
        let (alice_hex, nest, backend) = setup();
        backend.expect_replica_restore();
        backend.abandon_replica_restore();
        assert_eq!(
            backend
                .ensure_keypackages(2)
                .await
                .expect("an abandoned restore leaves no swap to strand the keys"),
            2,
        );
        assert_eq!(nest.keypackage_count_for(&alice_hex), 2);
    }
}

// ── Slice 1: native UniFFI receive seam (`ConversationsSession`) ────────
//
// `ConversationsSession` is the native (Apple/Android/Windows/Linux) twin of the
// wasm `WasmConversationsManager`'s FaunaMls half: it constructs a FaunaMls-wired
// `ConversationsManager` (via `from_parts`, the DI constructor tests pass a
// `MockNest` to) and drives the shared-Rust receive path — `ingest_welcome` +
// `poll_conversations` — that the web app reaches through `future_to_promise`
// wrappers (`docs/goal/ui/conversations.md` § State & data shape, § Architectural
// rules #2). These exercise the SESSION's own new logic: its per-channel cursor
// persistence and `bound_channels()` iteration (the underlying free functions are
// already covered by the Track B/C tests above).

/// `poll_conversations` ingests every queued message on a bound channel on the
/// first pass and advances each channel's cursor, so a second pass with no new
/// traffic ingests nothing. Drives the session's `conv_cursors` persistence +
/// `bound_channels()` iteration. The channel is bound the production way — via the
/// session's own `ingest_welcome` — so the test needs no test-only backend handle.
#[tokio::test]
async fn session_poll_conversations_advances_per_channel_cursor() {
    // Alice bootstraps a 1:1 with Bob (delivering a Welcome) and posts THREE
    // messages through the seam.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let alice_thread = fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]);
    for body in ["one", "two", "three"] {
        alice_backend
            .send(
                &alice_thread,
                &ComposeState {
                    body_draft: body.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("alice send ok");
    }

    // Bob's session over the same MockNest; bind the channel via the production
    // `ingest_welcome` path (no test-only backend access).
    let session =
        ConversationsSession::from_parts(bob.clone(), nest.clone(), "bob".into(), bob_actor, None);
    let manager = session.manager();
    let welcome = nest.welcomes()[0].clone();
    let thread_id = session
        .ingest_welcome(welcome.channel_hex.clone(), welcome.welcome_bytes.clone())
        .await
        .expect("welcome ingest")
        .expect("thread id");

    // First poll ingests all three; the cursor is now past them.
    let first = session.poll_conversations().await.expect("poll ok");
    assert_eq!(first, 3, "first pass ingests every queued message");
    let detail = manager
        .thread_detail(ThreadId(thread_id.clone()))
        .expect("thread");
    let bodies: Vec<&str> = detail.messages.iter().map(|m| m.body.as_str()).collect();
    assert!(
        ["one", "two", "three"].iter().all(|b| bodies.contains(b)),
        "all three decrypted into the bound thread; got {bodies:?}"
    );

    // Second poll with no new traffic ingests nothing — the cursor advanced.
    let second = session.poll_conversations().await.expect("poll ok");
    assert_eq!(
        second, 0,
        "session cursor prevents re-ingest on a quiet poll"
    );
}

/// A session built via `from_parts` ingests a Welcome (returns a thread id, binds
/// the channel), then `poll_conversations` routes that channel's messages into the
/// materialized thread — the end-to-end native receive path with no manual thread
/// setup.
#[tokio::test]
async fn session_ingest_welcome_then_poll_routes_into_materialized_thread() {
    // Alice bootstraps a 1:1 with Bob and sends "ping" (full Track-B send path).
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    // Bob's session: feed the delivered Welcome to the session's `ingest_welcome`.
    let session =
        ConversationsSession::from_parts(bob.clone(), nest.clone(), "bob".into(), bob_actor, None);
    let manager = session.manager();
    let welcome = nest.welcomes()[0].clone();
    let thread_id = session
        .ingest_welcome(welcome.channel_hex.clone(), welcome.welcome_bytes.clone())
        .await
        .expect("welcome ingest")
        .expect("a thread id is returned");

    // The materialized thread is FaunaMls and carries Alice as a participant.
    let detail = manager
        .thread_detail(ThreadId(thread_id.clone()))
        .expect("thread");
    assert_eq!(detail.rail, Rail::FaunaMls);
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "Alice is a participant of the materialized thread"
    );

    // The welcome bound the channel, so `poll_conversations` (no manual bind)
    // routes Alice's "ping" into the materialized thread.
    let ingested = session.poll_conversations().await.expect("poll ok");
    assert_eq!(ingested, 1, "the bound channel's message is ingested");
    let detail = manager.thread_detail(ThreadId(thread_id)).expect("thread");
    assert!(detail.messages.iter().any(|m| m.body == "ping"));
}

/// `ingest_welcome` is idempotent: re-ingesting the same Welcome returns the same
/// thread id and creates no duplicate thread (the channel is already bound, so the
/// key-package-consuming join is short-circuited).
#[tokio::test]
async fn session_ingest_welcome_is_idempotent() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    let session =
        ConversationsSession::from_parts(bob.clone(), nest.clone(), "bob".into(), bob_actor, None);
    let manager = session.manager();
    let welcome = nest.welcomes()[0].clone();

    let first = session
        .ingest_welcome(welcome.channel_hex.clone(), welcome.welcome_bytes.clone())
        .await
        .expect("first ingest")
        .expect("thread id");
    let again = session
        .ingest_welcome(welcome.channel_hex.clone(), welcome.welcome_bytes.clone())
        .await
        .expect("idempotent ingest")
        .expect("thread id");

    assert_eq!(
        again, first,
        "re-delivered welcome reuses the same thread id"
    );
    assert_eq!(
        manager.snapshot().threads.len(),
        1,
        "no duplicate thread on re-delivery"
    );
}

// ── Slice 3: the shared-Rust push-driven receive loop (`start_receive_loop`) ──
//
// `start_receive_loop` is the native twin of the linux `conv_backend.rs`
// `tokio::select!` loop: a detached task that drives the session's `ConversationsPush`
// seam — `welcome.received` → `ingest_welcome` then poll the now-bound channel,
// `channel.message` → poll — plus a backstop ticker, all into the wired manager
// (`docs/goal/ui/conversations.md` § Receiving into the conversations view,
// § MLS Welcome at-rest). Windows is the conversations lead app; this is the
// shared seam apple + android adopt after. These exercise the SESSION's loop
// wiring (the `ingest_welcome` / `poll_inbound_conv` free fns are already covered
// by the Track B/C + Slice-1 tests above), so a mock push stands in for the nest's
// WS push plane.

/// A test [`ConversationsPush`] that replays a fixed queue of push events, then
/// quiesces (sleeps, never returning a spurious event or a close) so the spawned
/// receive loop stays alive after draining the queue — the mock twin of a live
/// nest WS push subscription that has simply gone quiet.
struct MockPush {
    events: Mutex<VecDeque<ConvPushEvent>>,
}

#[async_trait]
impl ConversationsPush for MockPush {
    async fn next_event(&self) -> Option<ConvPushEvent> {
        loop {
            if let Some(ev) = self.events.lock().unwrap().pop_front() {
                return Some(ev);
            }
            // Queue drained: quiesce without closing the push (the loop's ticker
            // backstop keeps running; the lock is never held across the await).
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
}

/// `start_receive_loop` spawns a detached task that, fed a `welcome.received`
/// push for a channel that already carries an application message, joins +
/// materializes the thread (`ingest_welcome`) and then polls the now-bound
/// channel — so the pushed message lands in the manager with no manual
/// `ingest_welcome`/`poll` call. The native twin of the linux receive loop's
/// welcome arm (`conv_backend.rs`).
#[tokio::test(start_paused = true)]
async fn session_receive_loop_ingests_a_pushed_welcome() {
    // Alice bootstraps a 1:1 with Bob (delivering a Welcome) and sends "ping" —
    // the message is queued on the channel before Bob ever joins.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    // Bob's session over the same MockNest, with a push source seeded with the one
    // delivered Welcome (the `channel.message` poll falls out of the welcome arm).
    let welcome = nest.welcomes()[0].clone();
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::from([ConvPushEvent::Welcome(WelcomeNudge {
            channel_id_hex: Some(welcome.channel_hex.clone()),
            welcome_bytes: welcome.welcome_bytes.clone(),
            kind: WelcomeChannelKind::Dm,
            home_nest_url: None,
            shared_by: None,
            set_name: None,
            access: None,
            home_nest_actor_id: None,
            set_name_seal: None,
            shared_by_handle: None,
            shared_by_domain: None,
        })])),
    });
    let session = ConversationsSession::from_parts(
        bob.clone(),
        nest.clone(),
        "bob".into(),
        bob_actor,
        Some(push),
    );
    let manager = session.manager();

    // Start the detached receive loop; it owns the ingest + poll from here.
    session.start_receive_loop().await;

    // Bounded wait for the loop to materialize the thread and route "ping" in.
    let mut found: Option<ThreadDetail> = None;
    for _ in 0..200 {
        if let Some(summary) = manager.snapshot().threads.first().cloned()
            && let Some(detail) = manager.thread_detail(summary.thread_id)
            && detail.messages.iter().any(|m| m.body == "ping")
        {
            found = Some(detail);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let detail =
        found.expect("receive loop materialized the thread and ingested the pushed message");
    assert_eq!(detail.rail, Rail::FaunaMls);
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "Alice is a participant of the loop-materialized thread"
    );
}

/// A test [`InboxDrainSource`] that, on each `drain_once`, applies a fixed Welcome
/// through the session's shared ingest ([`ConversationsSession::ingest_welcome_by_kind`])
/// — the mock twin of the glue `NestInboxDrainSource`, which fetches the durable
/// `fauna.inbox.*` queue, decodes the canonical envelope, and routes a Welcome to
/// that same method. Lets the receive loop's ticker exercise the missed-push
/// backstop (layer 3) without a live nest queue. Holds a `Weak` session (the drain
/// twin of how the real source avoids the session→source Arc cycle).
struct MockInboxDrain {
    session: std::sync::Weak<ConversationsSession>,
    welcome: WelcomeNudge,
    calls: Arc<Mutex<usize>>,
}

#[async_trait]
impl InboxDrainSource for MockInboxDrain {
    async fn drain_once(&self) -> Result<(), String> {
        *self.calls.lock().unwrap() += 1;
        let Some(session) = self.session.upgrade() else {
            return Ok(());
        };
        let Some(channel_hex) = self.welcome.channel_id_hex.clone() else {
            return Ok(());
        };
        session
            .ingest_welcome_by_kind(
                self.welcome.kind.clone(),
                channel_hex,
                self.welcome.welcome_bytes.clone(),
                self.welcome.home_nest_url.clone().unwrap_or_default(),
                FolderWelcomeContext {
                    shared_by: self.welcome.shared_by.clone(),
                    set_name: self.welcome.set_name.clone(),
                    access: self.welcome.access.clone(),
                    home_nest_actor_id: self.welcome.home_nest_actor_id.clone(),
                    set_name_seal: None,
                    shared_by_handle: None,
                    shared_by_domain: None,
                },
            )
            .await
            .map_err(|e| e.to_string())
    }
}

/// `start_receive_loop`'s ticker arm drives the registered [`InboxDrainSource`]
/// **before** the channel sweep, so a Welcome whose best-effort push was **missed**
/// (here: Bob's session has NO push source) is recovered from the durable queue and
/// applied, and the same tick's `poll_bound` pulls the pre-join history in — the
/// missed-push delivery guarantee (`docs/goal/architecture/api-layers.md` § Inbox &
/// Messaging, layer 3).
#[tokio::test(start_paused = true)]
async fn session_receive_loop_drains_missed_welcome() {
    // Alice bootstraps a 1:1 with Bob (delivering a Welcome) and sends "ping" — the
    // message is queued on the channel before Bob ever joins.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");
    let welcome = nest.welcomes()[0].clone();

    // Bob's session over the same MockNest with **no push** — the missed-push case.
    // Only the ticker backstop + the registered drain can deliver the Welcome.
    let session =
        ConversationsSession::from_parts(bob.clone(), nest.clone(), "bob".into(), bob_actor, None);
    let manager = session.manager();

    let calls = Arc::new(Mutex::new(0usize));
    session.register_inbox_drain(Arc::new(MockInboxDrain {
        session: Arc::downgrade(&session),
        welcome: WelcomeNudge {
            channel_id_hex: Some(welcome.channel_hex.clone()),
            welcome_bytes: welcome.welcome_bytes.clone(),
            kind: WelcomeChannelKind::Dm,
            home_nest_url: None,
            shared_by: None,
            set_name: None,
            access: None,
            home_nest_actor_id: None,
            set_name_seal: None,
            shared_by_handle: None,
            shared_by_domain: None,
        },
        calls: calls.clone(),
    }));

    // Start the detached loop; the first (immediate) ticker tick drains the missed
    // Welcome, binds the channel, then the same tick's poll_bound ingests "ping".
    session.start_receive_loop().await;

    let mut found: Option<ThreadDetail> = None;
    for _ in 0..200 {
        if let Some(summary) = manager.snapshot().threads.first().cloned()
            && let Some(detail) = manager.thread_detail(summary.thread_id)
            && detail.messages.iter().any(|m| m.body == "ping")
        {
            found = Some(detail);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let detail = found.expect("drain backstop recovered the missed Welcome and ingested 'ping'");
    assert_eq!(detail.rail, Rail::FaunaMls);
    assert!(
        *calls.lock().unwrap() >= 1,
        "the ticker drove the drain backstop at least once"
    );
    assert!(
        detail
            .participants
            .iter()
            .any(|p| matches!(p, TypedAddress::Fauna { actor_id, .. } if *actor_id == alice_actor)),
        "Alice is a participant of the drain-materialized thread"
    );
}

/// [`ConversationsSession::ingest_welcome_by_kind`] — the single dispatch the push
/// arm and the drain backstop both route through — is idempotent: a Welcome
/// delivered via BOTH paths (modeled here as a second ingest of the same bytes)
/// re-binds the same thread, not a duplicate. This is the load-bearing property
/// that makes a Welcome arriving via push *and* the drain safe (NEXT task 3).
#[tokio::test]
async fn ingest_welcome_by_kind_is_idempotent() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");
    let welcome = nest.welcomes()[0].clone();

    let session =
        ConversationsSession::from_parts(bob.clone(), nest.clone(), "bob".into(), bob_actor, None);
    let manager = session.manager();

    // First delivery (e.g. via the push arm) binds one thread.
    session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Dm,
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext::default(),
        )
        .await
        .expect("first ingest binds the thread");
    assert_eq!(
        manager.snapshot().threads.len(),
        1,
        "first ingest bound exactly one thread"
    );

    // Second delivery (e.g. via the drain backstop) of the SAME Welcome is a no-op —
    // the MLS engine + channel binding dedup; no second join, no duplicate thread.
    session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Dm,
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext::default(),
        )
        .await
        .expect("re-ingest is a no-op, not an error");
    assert_eq!(
        manager.snapshot().threads.len(),
        1,
        "re-ingesting the same Welcome did not duplicate the thread"
    );
}

// ── Slice 2: native dual-rail session (`register_smtp`) ────────────────
//
// The native session is the twin of the wasm wrapper's dual-rail
// `with_conversations`: `from_parts` registers the FaunaMls rail, and
// `register_smtp` wires the SMTP rail with an injected `OutboundMailSink` (the
// native twin of the second `register_backend`). The sink itself lives in the
// transport layer (`fauna-ffi` over `EmailClient<Arc<NestClient>>`); the crate
// stays transport-free, so this test injects a recording sink.

/// After `register_smtp`, composing a new email thread and Sending routes the
/// assembled RFC 5322 through the injected sink: `dm-send-button →
/// manager.send_new_thread() → SmtpBackend::send → OutboundMailSink::submit`
/// (`docs/goal/ui/conversations.md` § User actions). Without the SMTP rail the
/// email recipient never resolves to a chip, so this captures the
/// previously-dead Send path end-to-end at the shared level.
#[tokio::test]
async fn session_register_smtp_routes_new_email_thread_through_sink() {
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
    }
    #[async_trait]
    impl OutboundMailSink for RecordingSink {
        async fn submit(
            &self,
            recipients: Vec<String>,
            raw_rfc5322: Vec<u8>,
        ) -> Result<(), String> {
            self.sent.lock().unwrap().push((recipients, raw_rfc5322));
            Ok(())
        }
    }

    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    let session = ConversationsSession::from_parts(
        engine,
        nest.clone(),
        "alice@fauna.test".into(),
        actor,
        None,
    );

    // The native twin of the wasm wrapper's second `register_backend` call.
    let sink = Arc::new(RecordingSink::default());
    session.register_smtp(sink.clone());

    // Compose + Send a new email thread, exactly the new-thread `dm-send-button`
    // flow (the picker holds a committed chip before send_new_thread).
    let m = session.manager();
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@example.com".into());
    m.resolve_recipient().await;
    assert!(
        m.accept_current_recipient_chip(),
        "the SMTP rail resolves bob@example.com into a committed chip"
    );
    m.set_new_thread_body("hello from the session".into());

    let new_id = m.send_new_thread().await.expect("send ok");
    assert!(new_id.is_some(), "a new thread is materialized + selected");

    let captured = sink.sent.lock().unwrap();
    assert_eq!(
        captured.len(),
        1,
        "exactly one mail submitted through the sink"
    );
    assert_eq!(
        captured[0].0,
        vec!["bob@example.com".to_string()],
        "envelope recipient is the resolved chip"
    );
    let raw = String::from_utf8_lossy(&captured[0].1);
    assert!(
        raw.contains("hello from the session"),
        "composed body present in the assembled RFC 5322; got:\n{raw}"
    );
}

/// conversations.md § Self-address: live, never baked — one
/// `ConversationsSession::set_self_address` heals BOTH rails of a session built
/// before identity resolution (the split-construction shape every app now
/// uses): the SMTP rail's next send carries the resolved `From:` instead of
/// refusing `no_handle` forever, and the FaunaMls rail's `self_address()` (the
/// reply-all self-drop + sender attribution) answers the resolved handle.
#[tokio::test]
async fn set_self_address_heals_a_session_built_before_identity_resolved() {
    #[derive(Default)]
    struct RecordingSink {
        sent: Mutex<Vec<(Vec<String>, Vec<u8>)>>,
    }
    #[async_trait]
    impl OutboundMailSink for RecordingSink {
        async fn submit(
            &self,
            recipients: Vec<String>,
            raw_rfc5322: Vec<u8>,
        ) -> Result<(), String> {
            self.sent.lock().unwrap().push((recipients, raw_rfc5322));
            Ok(())
        }
    }

    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    // The race-window build: the actor secret exists, the handle hasn't
    // resolved yet — construction must not wait for it (user ruling 1,
    // 2026-07-23).
    let session = ConversationsSession::from_parts(engine, nest.clone(), "".into(), actor, None);
    let sink = Arc::new(RecordingSink::default());
    session.register_smtp(sink.clone());

    let m = session.manager();
    m.start_new_conversation();
    m.set_new_thread_recipient_input("bob@example.com".into());
    m.resolve_recipient().await;
    assert!(m.accept_current_recipient_chip());
    m.set_new_thread_body("written before the handle resolved".into());
    m.send_new_thread()
        .await
        .expect_err("unresolved self-address refuses locally (no_handle floor)");
    assert!(sink.sent.lock().unwrap().is_empty());
    // The refusal happened AFTER the thread materialized (compose moved onto
    // the thread's draft, thread selected) — exactly the state a user retries
    // from once their handle resolves.
    let thread_id = m
        .snapshot()
        .selected_thread_id
        .expect("the refused send left the materialized thread selected");

    // Identity resolves (login refresh / background challenge / handle rename)
    // → the ONE setter heals the live session; nothing is rebuilt.
    session.set_self_address("alice@nest-a.test".into());

    m.send(thread_id).await.expect("send ok after resolution");
    let captured = sink.sent.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let raw = String::from_utf8_lossy(&captured[0].1);
    assert!(
        raw.contains("From: alice@nest-a.test"),
        "the SMTP rail reads the resolved address at send time; got:\n{raw}"
    );
    drop(captured);

    // The FaunaMls rail answers the same resolved address (reply-all self-drop
    // + outbound sender attribution read this).
    assert_eq!(
        session.backend().self_address(),
        Some(fauna_addr("alice@nest-a.test", actor))
    );
}

/// conversations.md § Self-address: live, never baked — the FaunaMls data plane
/// compares a peer's handle domain against OURS to route same-nest vs.
/// federation relay. A session built before identity resolution has an empty
/// self-domain, so every domain-carrying same-nest peer mis-routes as foreign;
/// once the address lands in the live cell, the next bootstrap routes same-nest.
#[tokio::test]
async fn set_self_address_heals_mls_same_nest_routing() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();

    let cell = fauna_conversations::backend::SelfAddress::new("");
    let backend =
        FaunaMlsBackend::new_shared(alice.clone(), nest.clone(), Arc::clone(&cell), alice_actor);

    // Race window: empty self-domain → bob@nest-a.test looks foreign and the
    // Welcome mis-routes through the federation relay.
    let compose = ComposeState {
        body_draft: "ping".into(),
        ..Default::default()
    };
    let thread = fauna_mls_thread(
        ThreadId("t-pre-resolve".into()),
        vec![fauna_addr("bob@nest-a.test", bob_actor)],
    );
    backend.send(&thread, &compose, &[]).await.expect("send ok");
    assert_eq!(
        nest.welcomes()[0].peer_domain,
        Some("nest-a.test".to_string()),
        "unresolved self-domain mis-routes a same-nest peer as foreign — the gap"
    );

    // The address lands → a new bootstrap routes the SAME domain same-nest.
    cell.set("alice@nest-a.test");
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let thread = fauna_mls_thread(
        ThreadId("t-post-resolve".into()),
        vec![fauna_addr("bob@nest-a.test", bob_actor)],
    );
    backend.send(&thread, &compose, &[]).await.expect("send ok");
    assert_eq!(
        nest.welcomes()[1].peer_domain,
        None,
        "the live cell heals routing: same domain → same-nest, no relay"
    );
}

/// After `register_mail_receive`, `poll_mail` drives BOTH the `INBOX` and `Sent`
/// read-feeds through the shared `poll_inbound_mail` driver into the manager:
/// `register_mail_receive(inbox, sent) → poll_mail() → InboundMailSource::fetch →
/// ConversationsManager::ingest_inbound` (`docs/goal/ui/conversations.md` §
/// Receiving into the conversations view — nest-backed mail reads both mailboxes
/// so own-MUA-sent mail surfaces too). The native session twin of the linux
/// `start_inbound_poll` loop; the FFI factory injects `NestMailInboundSource`s
/// here, this test injects fixed ones. The decrypt/bucket internals are already
/// covered by `smtp_backend_tests`; this asserts the SESSION wires the sources and
/// reaches the manager.
#[tokio::test]
async fn session_poll_mail_ingests_inbox_and_sent_feeds() {
    /// A no-op outbound sink — `register_smtp` must run first so the `Smtp`
    /// RailBackend exists for inbound mail to bucket against; this test exercises
    /// only the receive path, so the sink is never called.
    struct NoopSink;
    #[async_trait]
    impl OutboundMailSink for NoopSink {
        async fn submit(&self, _r: Vec<String>, _m: Vec<u8>) -> Result<(), String> {
            Ok(())
        }
    }

    /// A fixed `InboundMailSource` returning its records once (those with
    /// `uid > after_uid`), then nothing — the mock twin of a nest read-feed that
    /// has been fully drained. Records carry already-decrypted RFC 5322 (the real
    /// source decrypts behind the seam).
    struct FixedMail {
        records: Vec<InboundMailRecord>,
    }
    #[async_trait]
    impl InboundMailSource for FixedMail {
        async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
            let records: Vec<_> = self
                .records
                .iter()
                .filter(|r| r.uid > after_uid)
                .cloned()
                .collect();
            Ok(InboundMailPage {
                skipped: Vec::new(),
                highest_modseq: 0,
                records,
                more: false,
            })
        }
    }

    fn mail_record(
        uid: u32,
        id: &[u8],
        from: &str,
        subject: &str,
        body: &str,
    ) -> InboundMailRecord {
        let raw =
            format!("From: {from}\r\nTo: alice@fauna.test\r\nSubject: {subject}\r\n\r\n{body}\r\n");
        InboundMailRecord {
            uid,
            message_id: id.to_vec(),
            internal_date_ms: (uid as i64) * 1000,
            rfc5322: raw.into_bytes(),
            mailbox: fauna_conversations::backend::MailFeed::Inbox,
            suppress_from_view: false,
            has_seen_flag: false,
        }
    }

    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    let session = ConversationsSession::from_parts(
        engine,
        nest.clone(),
        "alice@fauna.test".into(),
        actor,
        None,
    );
    // The mail RailBackend must exist for inbound mail to bucket into threads.
    session.register_smtp(Arc::new(NoopSink));

    session.register_mail_receive(
        Arc::new(FixedMail {
            records: vec![mail_record(
                1,
                b"in-1",
                "bob@example.com",
                "inbound",
                "hi from bob",
            )],
        }),
        Arc::new(FixedMail {
            records: vec![mail_record(
                1,
                b"sent-1",
                "alice@fauna.test",
                "outbound",
                "my own sent copy",
            )],
        }),
    );

    let n = session.poll_mail().await.expect("poll_mail ok");
    assert_eq!(n, 2, "one INBOX + one Sent message ingested");

    let threads = session.manager().snapshot().threads;
    assert_eq!(
        threads.len(),
        2,
        "both the inbound and the own-sent mail materialized a thread"
    );

    // A second poll is a no-op — the per-mailbox cursor advanced past both records.
    let again = session.poll_mail().await.expect("second poll ok");
    assert_eq!(again, 0, "cursors advanced; nothing re-ingested");
}

/// `start_receive_loop` reacts to a `fauna.mail.received` wake by polling the mail
/// read-feeds promptly — the shared twin of linux's `mail_sink.rs::start_inbound_poll`
/// `subscribe_kind("fauna.mail.received")` arm (`docs/goal/behavior/smtp-server.md` §
/// Inbound client receive → Arrival push). This is what lets the native apps
/// (windows/macos/ios/android, via the FFI factory's `NestConversationsPush`) render
/// a delivered message promptly instead of within one ticker cycle.
///
/// The mail source is **armed-gated**: it returns its record only after `arm()`, so
/// the loop's immediate first ticker tick fetches empty. Arming happens AFTER that
/// first tick, so the record is delivered ONLY if the `ConvPushEvent::MailReceived`
/// wake triggers a later poll — the next ticker tick is the production interval (30s)
/// away, far past the bounded wait, proving push-not-tick delivery. On the virtual
/// clock (`start_paused`) that ordering is deterministic rather than raced: the
/// 300ms gate and the delivery poll both resolve in virtual time strictly before
/// the 30s tick can fire.
#[tokio::test(start_paused = true)]
async fn session_receive_loop_reacts_to_mail_push() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct NoopSink;
    #[async_trait]
    impl OutboundMailSink for NoopSink {
        async fn submit(&self, _r: Vec<String>, _m: Vec<u8>) -> Result<(), String> {
            Ok(())
        }
    }

    /// Returns its single record only once `armed` is set (and only with
    /// `after_uid` below it) — empty otherwise. Models a read-feed that has nothing
    /// to hand back until a message actually arrives.
    struct ArmedMail {
        armed: AtomicBool,
        record: InboundMailRecord,
    }
    #[async_trait]
    impl InboundMailSource for ArmedMail {
        async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
            if !self.armed.load(Ordering::SeqCst) || after_uid >= self.record.uid {
                return Ok(InboundMailPage::default());
            }
            Ok(InboundMailPage {
                skipped: Vec::new(),
                highest_modseq: 0,
                records: vec![self.record.clone()],
                more: false,
            })
        }
    }

    /// Empty Sent feed — this test exercises only the INBOX/push path.
    struct EmptyMail;
    #[async_trait]
    impl InboundMailSource for EmptyMail {
        async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
            Ok(InboundMailPage::default())
        }
    }

    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());

    // A push source we drive by hand: starts empty (so the loop parks on it), then
    // we enqueue the `MailReceived` wake after arming the source.
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::new()),
    });
    let session = ConversationsSession::from_parts(
        engine,
        nest.clone(),
        "alice@fauna.test".into(),
        actor,
        Some(push.clone()),
    );
    session.register_smtp(Arc::new(NoopSink));

    let inbox = Arc::new(ArmedMail {
        armed: AtomicBool::new(false),
        record: InboundMailRecord {
            uid: 1,
            message_id: b"in-1".to_vec(),
            internal_date_ms: 1000,
            rfc5322: b"From: bob@example.com\r\nTo: alice@fauna.test\r\nSubject: ping\r\n\r\nping body\r\n".to_vec(),
            mailbox: fauna_conversations::backend::MailFeed::Inbox, suppress_from_view: false, has_seen_flag: false,
        },
    });
    session.register_mail_receive(inbox.clone(), Arc::new(EmptyMail));

    session.start_receive_loop().await;

    // Let the immediate first ticker tick fire + fetch (empty — not yet armed), so a
    // later delivery can only come from the push. Virtual time: this resolves
    // instantly and the first tick is guaranteed to have run, not hoped for.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        session.manager().snapshot().threads.is_empty(),
        "no mail before arming — the first tick fetched an empty feed"
    );

    // Arm the feed and fire the arrival wake; the push arm must poll mail promptly.
    inbox.armed.store(true, Ordering::SeqCst);
    push.events
        .lock()
        .unwrap()
        .push_back(ConvPushEvent::MailReceived);

    let mut delivered = false;
    for _ in 0..200 {
        if !session.manager().snapshot().threads.is_empty() {
            delivered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        delivered,
        "the `fauna.mail.received` wake drove a prompt mail poll (not the 30s ticker)"
    );
}

/// `start_receive_loop` reacts to a `ConvPushEvent::Reconnected` wake by running the
/// **full sweep** — the durable inbox drain AND the rails' cursor polls — exactly as
/// a ticker tick does.
///
/// Why the reconnect needs its own wake at all: a push is a *transient* broadcast,
/// so anything the nest tried to deliver while the socket was down was never
/// broadcast to us and only a pull recovers it. Conversations is the surface this
/// was missing — every other live surface (feed / knocks / contacts / notifications
/// / account) already re-pulls on `NestClient::subscribe_reconnects`, while this
/// loop relied on its 30 s backstop ticker alone, leaving MLS delivery up to one
/// full tick behind every other surface after a flap (`transport.md` § Reconnect &
/// resync).
///
/// Both gates open only AFTER the loop's immediate first ticker tick has run and
/// found nothing, and the next tick is the production interval (30 s) away — far
/// past this bounded wait — so anything delivered here came from the reconnect wake,
/// not the ticker.
///
/// The **two-rail** assertion is the point, not belt-and-braces: a mapping that
/// swept only mail (or only the channels) would satisfy half of this and still be
/// wrong, so each rail alone is too weak to pin "full sweep".
#[tokio::test(start_paused = true)]
async fn session_receive_loop_sweeps_every_rail_on_reconnect() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct NoopSink;
    #[async_trait]
    impl OutboundMailSink for NoopSink {
        async fn submit(&self, _r: Vec<String>, _m: Vec<u8>) -> Result<(), String> {
            Ok(())
        }
    }

    /// Counts drains and applies nothing — this test pins that the sweep *ran*; the
    /// drain's own apply path is covered by `session_receive_loop_drains_missed_welcome`.
    struct CountingDrain {
        calls: AtomicUsize,
    }
    #[async_trait]
    impl InboxDrainSource for CountingDrain {
        async fn drain_once(&self) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// Hands back its record only once `armed` — models a read-feed with nothing to
    /// return until a message actually lands (here: while the socket was down).
    struct ArmedMail {
        armed: AtomicBool,
        record: InboundMailRecord,
    }
    #[async_trait]
    impl InboundMailSource for ArmedMail {
        async fn fetch(&self, after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
            if !self.armed.load(Ordering::SeqCst) || after_uid >= self.record.uid {
                return Ok(InboundMailPage::default());
            }
            Ok(InboundMailPage {
                skipped: Vec::new(),
                highest_modseq: 0,
                records: vec![self.record.clone()],
                more: false,
            })
        }
    }

    struct EmptyMail;
    #[async_trait]
    impl InboundMailSource for EmptyMail {
        async fn fetch(&self, _after_uid: u32, _limit: u32) -> Result<InboundMailPage, String> {
            Ok(InboundMailPage::default())
        }
    }

    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let nest = Arc::new(MockNest::default());

    // Empty queue: the loop parks on the push arm until we hand it the wake.
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::new()),
    });
    let session = ConversationsSession::from_parts(
        engine,
        nest.clone(),
        "alice@fauna.test".into(),
        actor,
        Some(push.clone()),
    );
    session.register_smtp(Arc::new(NoopSink));

    let drain = Arc::new(CountingDrain {
        calls: AtomicUsize::new(0),
    });
    session.register_inbox_drain(drain.clone());

    let inbox = Arc::new(ArmedMail {
        armed: AtomicBool::new(false),
        record: InboundMailRecord {
            uid: 1,
            message_id: b"reconnect-1".to_vec(),
            internal_date_ms: 1000,
            rfc5322: b"From: bob@example.com\r\nTo: alice@fauna.test\r\nSubject: while-you-were-out\r\n\r\nbody\r\n".to_vec(),
            mailbox: fauna_conversations::backend::MailFeed::Inbox, suppress_from_view: false, has_seen_flag: false,
        },
    });
    session.register_mail_receive(inbox.clone(), Arc::new(EmptyMail));

    session.start_receive_loop().await;

    // Let the immediate first tick run against both closed gates. Virtual time:
    // resolves instantly, and the tick below is guaranteed rather than raced —
    // on the wall clock this assert could red spuriously under machine load.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let after_first_tick = drain.calls.load(Ordering::SeqCst);
    assert!(
        after_first_tick >= 1,
        "the immediate first ticker tick drives the drain (sanity: the loop is live)"
    );
    assert!(
        session.manager().snapshot().threads.is_empty(),
        "nothing before arming — the first tick swept empty feeds"
    );

    // Open both gates, then fire exactly ONE reconnect wake.
    inbox.armed.store(true, Ordering::SeqCst);
    push.events
        .lock()
        .unwrap()
        .push_back(ConvPushEvent::Reconnected);

    let mut mail_delivered = false;
    for _ in 0..200 {
        if !session.manager().snapshot().threads.is_empty() {
            mail_delivered = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Rail 1: the mail cursor poll ran.
    assert!(
        mail_delivered,
        "the reconnect wake swept the mail rail (the next 30s tick is far past this wait)"
    );
    // Rail 2: the durable inbox drain ran again — so this was the full sweep, not a
    // mail-only poll.
    assert!(
        drain.calls.load(Ordering::SeqCst) > after_first_tick,
        "the reconnect wake ran the durable inbox drain too — a full sweep, not a \
         mail-only poll (drain calls stayed at {after_first_tick})"
    );
}

/// `start_receive_loop`'s task holds only a `Weak` to the session's liveness
/// handle, so the loop exits once the owner drops the session — the linux
/// re-injection guard (the e2e session-cached driver rebuilds the session at every
/// re-injection; linux keeps the live one in a process static and drops the old, so
/// loops don't accumulate and re-ingest the same inbound every tick). Proven by a
/// counting INBOX source: after the session is dropped the loop handles at most one
/// more wake (the in-flight one) and then exits — so a burst of three pushes drives
/// at most one more poll, not three. Scoped per session, so concurrent sessions /
/// parallel tests never supersede each other.
#[tokio::test(start_paused = true)]
async fn start_receive_loop_stops_when_session_dropped() {
    use std::sync::atomic::{AtomicU64, Ordering};

    struct NoopSink;
    #[async_trait]
    impl OutboundMailSink for NoopSink {
        async fn submit(&self, _r: Vec<String>, _m: Vec<u8>) -> Result<(), String> {
            Ok(())
        }
    }
    struct EmptyMail;
    #[async_trait]
    impl InboundMailSource for EmptyMail {
        async fn fetch(&self, _a: u32, _l: u32) -> Result<InboundMailPage, String> {
            Ok(InboundMailPage::default())
        }
    }
    /// Counts every poll; always returns empty (so nothing ingests — we only
    /// measure how often the loop polled this source).
    struct CountingMail {
        polls: Arc<AtomicU64>,
    }
    #[async_trait]
    impl InboundMailSource for CountingMail {
        async fn fetch(&self, _a: u32, _l: u32) -> Result<InboundMailPage, String> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            Ok(InboundMailPage::default())
        }
    }

    // A session with a hand-driven push + a counting INBOX source.
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let actor = engine.identity_actor_id();
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::new()),
    });
    let session = ConversationsSession::from_parts(
        engine,
        Arc::new(MockNest::default()),
        "a@fauna.test".into(),
        actor,
        Some(push.clone()),
    );
    session.register_smtp(Arc::new(NoopSink));
    let polls = Arc::new(AtomicU64::new(0));
    session.register_mail_receive(
        Arc::new(CountingMail {
            polls: polls.clone(),
        }),
        Arc::new(EmptyMail),
    );
    session.start_receive_loop().await;

    // Wait for the immediate first ticker tick to poll the counting source once and
    // park on its select.
    for _ in 0..100 {
        if polls.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let before = polls.load(Ordering::SeqCst);
    assert!(before >= 1, "the loop polled on its first tick");

    // Drop the only strong ref to the session: the loop holds just a `Weak` to its
    // liveness handle, so its next loop-top check fails to upgrade and it exits.
    drop(session);

    // Fire three arrival wakes. The loop wakes once, polls (the in-flight iteration
    // completes), returns to the loop top, sees the dropped session, and exits —
    // leaving the other two pushes unprocessed in the queue.
    {
        let mut q = push.events.lock().unwrap();
        q.push_back(ConvPushEvent::MailReceived);
        q.push_back(ConvPushEvent::MailReceived);
        q.push_back(ConvPushEvent::MailReceived);
    }
    // Virtual time makes this a quiesce barrier, not a hope: the runtime only
    // advances the clock once every task is idle, so by the time this sleep
    // resolves, a still-alive loop would already have drained ALL THREE wakes —
    // on the wall clock a slow machine could reach the assert before the loop
    // processed anything, passing it vacuously.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    assert!(
        polls.load(Ordering::SeqCst) <= before + 1,
        "a loop whose session was dropped handles at most one more wake then stops — \
         three queued pushes drove at most one more poll, not three (no accumulation \
         across session re-injections); got {} (before {})",
        polls.load(Ordering::SeqCst),
        before,
    );
}

/// `from_manager` wires the session onto the caller's **existing** manager (not a
/// fresh internal one) — the constructor a Rust-native app (linux) uses to keep
/// its `host::manager()` singleton as the single observable surface while still
/// driving the shared receive loop. A welcome ingested through the session
/// materializes a thread on the **passed** manager handle, and `session.manager()`
/// is that same `Arc`.
#[tokio::test]
async fn session_from_manager_drives_the_caller_manager() {
    // Alice bootstraps a 1:1 with Bob (delivering a Welcome) and sends "ping".
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "ping".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+send");

    // Bob's session is built over a manager the caller already owns (the linux
    // singleton stand-in), NOT a fresh one.
    let my_manager = ConversationsManager::new();
    let session = ConversationsSession::from_manager(
        my_manager.clone(),
        bob.clone(),
        nest.clone(),
        "bob".into(),
        bob_actor,
        None,
    );
    assert!(
        Arc::ptr_eq(&session.manager(), &my_manager),
        "the session drives the caller's manager, not a fresh internal one"
    );

    let welcome = nest.welcomes()[0].clone();
    session
        .ingest_welcome(welcome.channel_hex.clone(), welcome.welcome_bytes.clone())
        .await
        .expect("ingest_welcome");
    session.poll_conversations().await.expect("poll");

    // The thread + message are observable on the CALLER's manager handle.
    let summary = my_manager
        .snapshot()
        .threads
        .first()
        .cloned()
        .expect("a thread materialized on the caller's manager");
    let detail = my_manager
        .thread_detail(summary.thread_id)
        .expect("thread detail on the caller's manager");
    assert!(
        detail.messages.iter().any(|m| m.body == "ping"),
        "the ingested message is visible on the manager the caller passed in"
    );
}

// ── Track E: send_reaction / send_delete — send-side round-trip ────────

#[tokio::test]
async fn send_reaction_posts_envelope_peer_decrypts() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel_id = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel_id);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    let thread_id = ThreadId("t-reaction".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .send_reaction(&thread_id, 42, "👍", ReactionOp::Add)
        .await
        .expect("send_reaction ok");

    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "exactly one channel.send");
    let mls_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application envelope"),
    };
    let decrypted = bob.decrypt(&channel_id, &mls_bytes).expect("peer decrypts");
    assert!(
        matches!(
            decrypted.body,
            ChannelMessageBody::Reaction { target_seq: 42, ref emoji, op: ReactionOp::Add } if emoji == "👍"
        ),
        "got {:?}",
        decrypted.body
    );
    assert_eq!(decrypted.sender, alice.identity_actor_id());
}

#[tokio::test]
async fn send_delete_posts_envelope_peer_decrypts() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let bob_channel_id = bob.join_from_welcome(welcome).unwrap();
    assert_eq!(channel_id, bob_channel_id);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    let thread_id = ThreadId("t-delete".into());
    backend.bind_channel(thread_id.clone(), channel_id);

    backend
        .send_delete(&thread_id, 7)
        .await
        .expect("send_delete ok");

    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "exactly one channel.send");
    let mls_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application envelope"),
    };
    let decrypted = bob.decrypt(&channel_id, &mls_bytes).expect("peer decrypts");
    assert!(
        matches!(decrypted.body, ChannelMessageBody::Delete { target_seq: 7 }),
        "got {:?}",
        decrypted.body
    );
}

// ── MLS-1 regression: the sender-only-delete floor holds against a forged
//    cross-sender delete (inner `sender` = victim) ──────────────────────────

/// Regression pin: the documented sender-only-delete floor
/// (`conversations.md` § Reactions & message delete: "a `Delete` sets `deleted`
/// only if `delete.sender == target.sender`") must hold against the
/// *sophisticated* forgery where an in-group member sets the inner `sender` of a
/// `Delete` to its victim. Because `engine::decrypt` binds the returned `sender`
/// to the MLS-authenticated leaf, the forged claim is attributed to the real
/// (attacker) sender — not the victim — so the floor drops it and the victim's
/// message survives. The naive forgery (leaving `sender` = the attacker) was
/// already dropped; this covers the `sender = victim` case the floor must also
/// withstand. Runs the production seam end-to-end (decrypt → poll_inbound_conv →
/// projection) in a 3-member group.
#[tokio::test]
async fn forged_cross_sender_delete_does_not_tombstone_victim_message() {
    // Alice = attacker (also creates the group), Bob = receiver, Carol = victim.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());

    // 3-member group: alice creates with bob + carol; both join the shared Welcome.
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    let bob_channel = bob.join_from_welcome_bytes(&welcome_bytes).unwrap();
    carol.join_from_welcome_bytes(&welcome_bytes).unwrap();
    assert_eq!(channel_id, bob_channel);

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let carol_actor = carol.identity_actor_id();

    // Bob's receive side: a manager + backend, thread bound to the channel
    // (seeded via a setup ingest, as in the Track-B inbound tests).
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("carol", carol_actor),
            recipients: vec![
                fauna_addr("bob", bob_actor),
                fauna_addr("alice", alice_actor),
            ],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    // (1) Carol posts a genuine message. It lands at channel sequence 1, so its
    // bubble id is `conv:{hex}:1` — the target the Delete will name. (Sealed via
    // the raw engine with an honest inner sender = Carol; the authenticated leaf
    // is also Carol, so it is correctly attributed either way.)
    let carol_real = ChannelMessage {
        sender: carol_actor,
        sequence: 1,
        channel_epoch: 0,
        body: ChannelMessageBody::Text("carol's real message".into()),
        timestamp: Timestamp::now(),
    };
    let carol_ct = carol.encrypt(&channel_id, &carol_real).unwrap();
    nest.channel_send(
        channel_id.to_string(),
        ChannelEnvelope::Application(carol_ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // (2) Alice — a real, authenticated member — forges a cooperative delete of
    // Carol's message, setting the inner `sender` to Carol to try to satisfy the
    // `claim == target.sender` floor.
    let forged_delete = ChannelMessage {
        sender: carol_actor, // forged: claim it is Carol deleting her own message
        sequence: 1,
        channel_epoch: 0,
        body: ChannelMessageBody::Delete { target_seq: 1 },
        timestamp: Timestamp::now(),
    };
    let forged_ct = alice.encrypt(&channel_id, &forged_delete).unwrap();
    nest.channel_send(
        channel_id.to_string(),
        ChannelEnvelope::Application(forged_ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // Bob drains the channel: ingests Carol's bubble, then the forged delete claim.
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok")
        .ingested;
    assert_eq!(
        ingested, 1,
        "one chat bubble (Carol's message); the Delete is a side-effect, not a bubble"
    );

    // Carol's message is present and NOT tombstoned: the forged delete's claim is
    // attributed to the *authenticated* sender (Alice), not Carol, so the
    // `claim == target.sender` floor drops it.
    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    let carol_bubble = detail
        .messages
        .iter()
        .find(|m| m.body == "carol's real message")
        .expect("carol's message bubble present");
    assert!(
        !carol_bubble.deleted,
        "a forged cross-sender delete (inner sender = victim) must NOT tombstone the victim's message"
    );
}

#[tokio::test]
async fn mls2_forged_welcome_rejected_at_ingest_materializes_no_thread() {
    // MLS-2: a patched inviter whose own
    // leaf credential names Carol (signed by Mallory's key) invites honest Bob.
    // Bob's welcome-ingest must reject the join — the ratchet tree carries a
    // forged-credential leaf — so NO thread materializes and the projection
    // layer never attributes a roster member or message to an identity that
    // doesn't own its leaf signature key. The companion to the engine-level
    // join/add/commit regressions in `fauna-mls`.
    let carol = ActorKeypair::generate().actor_id();
    let mallory = Arc::new(
        MlsEngine::new_in_memory_forged_for_test(ActorKeypair::generate(), carol).unwrap(),
    );
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_actor = bob.identity_actor_id();

    // Mallory (patched) builds the group + Welcome for Bob. create_group only
    // validates the *invited* (honest) KeyPackage, not Mallory's own leaf, so the
    // poisoned Welcome is produced — exactly the malicious-inviter vector.
    let bob_kp = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = mallory.create_group(&bob_kp).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();

    let nest = Arc::new(MockNest::default());
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());

    let res = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &channel_id.to_string(),
        &welcome_bytes,
        "",
    )
    .await;

    assert!(
        res.is_err(),
        "ingest_welcome must reject a Welcome whose roster carries a forged-credential leaf"
    );
    assert!(
        bob_manager.snapshot().threads.is_empty(),
        "no thread should materialize from a rejected forged Welcome (no impersonated roster)"
    );
}

// ── Folder recipient contact gate (folders.md § Sharing, Slice B2) ──────
//
// The receive rail routes a cross-user shared-folder `WelcomeChannelKind::Folder`
// welcome through a registered `FolderGateSink`, which returns an
// `ArrivalDisposition`; the rail then joins off the chat rail (Auto), stages the
// welcome un-acked (Knock → `Err` → retained by the durable drain), or drops it
// (Suppress). The gate's contacts read lives in the FFI/wasm glue
// (`NestFolderGate`); here a mock returns a fixed disposition.

/// A recipient contact gate returning a fixed [`ArrivalDisposition`] and recording
/// the `shared_by` it was handed (to prove the nest-stamped sharer id reaches it).
struct MockFolderGate {
    disposition: ArrivalDisposition,
    seen_shared_by: Mutex<Option<Option<String>>>,
    /// The `(group_id_hex, home_nest_url)` of every `drop_roster_row` call — the
    /// Suppress arm's roster drop (`folders.md` § Sharing → *Adding the 2nd..Nth
    /// member*: a suppressed knock drops the roster row like a decline).
    dropped_rosters: Mutex<Vec<(String, Option<String>)>>,
}

impl MockFolderGate {
    fn new(disposition: ArrivalDisposition) -> Self {
        Self {
            disposition,
            seen_shared_by: Mutex::new(None),
            dropped_rosters: Mutex::new(Vec::new()),
        }
    }

    fn seen_shared_by(&self) -> Option<Option<String>> {
        self.seen_shared_by.lock().unwrap().clone()
    }

    fn dropped_rosters(&self) -> Vec<(String, Option<String>)> {
        self.dropped_rosters.lock().unwrap().clone()
    }
}

#[async_trait]
impl FolderGateSink for MockFolderGate {
    async fn arrival_for(&self, shared_by: Option<String>) -> ArrivalDisposition {
        *self.seen_shared_by.lock().unwrap() = Some(shared_by);
        self.disposition
    }

    async fn drop_roster_row(&self, group_id_hex: String, home_nest_url: Option<String>) {
        self.dropped_rosters
            .lock()
            .unwrap()
            .push((group_id_hex, home_nest_url));
    }
}

/// Bootstrap a group Bob can join from a Welcome: Alice sends Bob one message,
/// lazily creating the MLS group + delivering a Welcome to the MockNest. The
/// Welcome bytes are group-agnostic — folder-ness is entirely in how Bob's side
/// routes them — so this reuses the ordinary send bootstrap. Returns the shared
/// `nest`, Bob's `engine` + `actor` (to build his session), Alice's actor hex (the
/// sharer id the gate reads), and the captured `welcome`.
async fn folder_welcome_setup() -> (Arc<MockNest>, Arc<MlsEngine>, ActorId, String, WelcomeCall) {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("fs-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "share".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice bootstrap+deliver welcome");
    let welcome = nest.welcomes()[0].clone();
    (nest, bob, bob_actor, hex::encode(alice_actor.0), welcome)
}

#[tokio::test]
async fn join_folder_welcome_joins_group_without_a_thread_and_is_idempotent() {
    let (nest, bob, bob_actor, _alice_hex, welcome) = folder_welcome_setup().await;
    let bob_backend = FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob_actor);

    let c1 = join_folder_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("first join");
    // A folder membership binds the MLS group only — not a chat thread and not a
    // scheduling channel — so content-key/chunk reads resolve but nothing surfaces
    // in the conversation UI.
    assert!(
        bob_backend.is_folder_channel(&c1),
        "marked a folder channel"
    );
    assert!(
        bob.has_group(&c1),
        "the MLS group is joined (content-key reads resolve)"
    );
    assert!(
        bob_backend.bound_channels().is_empty(),
        "no chat thread bound"
    );
    assert!(
        bob_backend.scheduling_channels().is_empty(),
        "not a scheduling channel"
    );

    // A re-delivered Welcome (the same share arrives via both the push arm and the
    // durable drain) must not attempt a second, init-key-spending join.
    let c2 = join_folder_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("second join is a no-op");
    assert_eq!(c1, c2, "idempotent re-join returns the same channel");
}

/// **Every Welcome join persists the `provider` blob before returning** —
/// `devices.md` § Durability rules, Rule 3 on a spent init key, for the two
/// thread-less joins (the chat join is pinned end to end in
/// `fauna-client-mls-sync::orchestration`): a folder and a scheduling Welcome
/// each drive the injected `ProviderPersist` seam exactly once, after the join.
#[tokio::test]
async fn folder_and_scheduling_welcome_joins_persist_the_provider() {
    use fauna_conversations::backend::ProviderPersist;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingProviderPersist {
        calls: AtomicUsize,
        engine: Arc<MlsEngine>,
        channel_hex: String,
        held_at_call: Mutex<Option<bool>>,
    }
    #[async_trait]
    impl ProviderPersist for CountingProviderPersist {
        async fn persist_provider(&self) -> Result<bool, BackendError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let channel = ChannelId::from_hex(&self.channel_hex).unwrap();
            *self.held_at_call.lock().unwrap() = Some(self.engine.has_group(&channel));
            Ok(true)
        }
    }

    // Folder.
    {
        let (nest, bob, bob_actor, _alice_hex, welcome) = folder_welcome_setup().await;
        let bob_backend = FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob_actor);
        let persist = Arc::new(CountingProviderPersist {
            calls: AtomicUsize::new(0),
            engine: bob.clone(),
            channel_hex: welcome.channel_hex.clone(),
            held_at_call: Mutex::new(None),
        });
        bob_backend.set_provider_persist(persist.clone());
        join_folder_welcome(
            &bob_backend,
            &welcome.channel_hex,
            &welcome.welcome_bytes,
            "",
            &FolderWelcomeContext::default(),
        )
        .await
        .expect("folder join");
        assert_eq!(
            persist.calls.load(Ordering::SeqCst),
            1,
            "the folder join persists the provider once"
        );
        assert_eq!(
            *persist.held_at_call.lock().unwrap(),
            Some(true),
            "…after the join, so the flushed provider carries the group"
        );
    }

    // Scheduling.
    {
        let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let bob_actor = bob.identity_actor_id();
        let nest = Arc::new(MockNest::default());
        nest.seed_keypackage(
            &hex::encode(bob_actor.0),
            bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
        );
        let alice_actor = alice.identity_actor_id();
        let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
        alice_backend
            .deliver_scheduling_imip(bob_actor, None, sample_imip())
            .await
            .expect("scheduling delivery ok");
        let welcome = nest.welcomes()[0].clone();
        let bob_backend = FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob_actor);
        let persist = Arc::new(CountingProviderPersist {
            calls: AtomicUsize::new(0),
            engine: bob.clone(),
            channel_hex: welcome.channel_hex.clone(),
            held_at_call: Mutex::new(None),
        });
        bob_backend.set_provider_persist(persist.clone());
        ingest_scheduling_welcome(
            &bob_backend,
            &welcome.channel_hex,
            &welcome.welcome_bytes,
            "",
        )
        .await
        .expect("scheduling join");
        assert_eq!(
            persist.calls.load(Ordering::SeqCst),
            1,
            "the scheduling join persists the provider once"
        );
        assert_eq!(*persist.held_at_call.lock().unwrap(), Some(true));
    }
}

/// The folder join arms the owner-managed-roster commit policy: the Welcome's
/// MLS-authenticated sender (the sharer — the owner, by the owner-only-Adds
/// invariant the policy preserves) is stamped as the channel's durable folder
/// owner, so `MlsEngine::process_commit` refuses non-owner roster changes on
/// this member's seat from the moment of the join (`federation.md` § Cross-nest
/// shared folders + channel append, re-ratified 2026-08-24).
#[tokio::test]
async fn join_folder_welcome_stamps_the_welcome_sender_as_folder_owner() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    let bob_backend = FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob_actor);

    let channel = join_folder_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("join");

    let owner = bob
        .folder_channel_owner(&channel)
        .expect("the join stamped a folder owner");
    assert_eq!(
        hex::encode(owner.0),
        alice_hex,
        "the stamped owner is the Welcome's MLS-authenticated sender (the sharer)"
    );
}

// ── the folder-owner marker follows the owner's verified succession
//    (`federation.md` § Cross-nest shared folders + channel append) ──

/// A folder channel alice owns with bob as its member — bob's seat stamped as
/// the folder-Welcome join stamps it — plus alice's successor and a genuine
/// statement for the pair: the seat every marker-succession case starts from.
struct FolderSuccessionSetup {
    alice: Arc<MlsEngine>,
    alice_actor: ActorId,
    bob: Arc<MlsEngine>,
    bob_backend: Arc<FaunaMlsBackend>,
    channel: ChannelId,
    nest: Arc<MockNest>,
    successor_engine: Arc<MlsEngine>,
    successor: ActorId,
    signed: fauna_core::recovery::SignedIdentitySuccession,
    head: fauna_core::recovery::ChainHead,
}

fn folder_succession_setup() -> FolderSuccessionSetup {
    folder_succession_setup_with(Arc::new(
        MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap(),
    ))
}

/// [`folder_succession_setup`] with bob's seat on the engine the caller
/// supplies — a persisted one when the case relaunches bob's session.
fn folder_succession_setup_with(bob: Arc<MlsEngine>) -> FolderSuccessionSetup {
    let alice_kp = ActorKeypair::from_secret([11u8; 32]);
    let successor_kp = ActorKeypair::from_secret([22u8; 32]);
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([11u8; 32])).unwrap());
    let successor_engine =
        Arc::new(MlsEngine::new_in_memory(ActorKeypair::from_secret([22u8; 32])).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();
    let alice_actor = alice.identity_actor_id();
    // The folder mint's and the folder-Welcome join's stamps (pinned by
    // `join_folder_welcome_stamps_the_welcome_sender_as_folder_owner` and the
    // adapter's own test); the policy is armed on bob's seat from here.
    alice.mark_folder_channel_owner(&channel, &alice_actor);
    bob.mark_folder_channel_owner(&channel, &alice_actor);

    let recovery = fauna_core::recovery::RecoveryKey::generate();
    let statement = fauna_core::recovery::IdentitySuccession {
        old_actor_id: alice_kp.actor_id(),
        new_actor_id: successor_kp.actor_id(),
        recovery_pubkey: recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    };
    let signed = statement
        .sign(
            &recovery,
            successor_kp.signing_key(),
            Some(alice_kp.signing_key()),
        )
        .expect("the fixture statement signs");
    let head = fauna_core::recovery::ChainHead::new(recovery.public(), 1);

    let nest = Arc::new(MockNest::default());
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    FolderSuccessionSetup {
        alice,
        alice_actor,
        bob,
        bob_backend,
        channel,
        nest,
        successor_engine,
        successor: successor_kp.actor_id(),
        signed,
        head,
    }
}

async fn poll_folder(s: &FolderSuccessionSetup, cursor: &mut i64) -> usize {
    poll_inbound_folder(&s.bob_backend, &s.channel, cursor, 50)
        .await
        .expect("the folder poll walks")
        .applied
}

/// The honest ceremony, at a member's seat: the statement rides beside the
/// add, so it verifies with the successor seated and the predecessor still
/// there — and the marker re-points **at that midpoint**, which is what admits
/// the successor-authored remove-old through the folder commit policy. After
/// it, the retired leaf is gone and the successor owns the channel on bob's
/// seat.
#[tokio::test]
async fn the_folder_rail_re_stamps_the_owner_marker_at_the_midpoint_and_admits_remove_old() {
    let s = folder_succession_setup();
    s.bob_backend
        .set_succession_witness(Arc::new(HeadWitness(s.head)));
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    poll_folder(&s, &mut cursor).await;
    assert_eq!(
        s.bob.folder_channel_owner(&s.channel),
        Some(s.successor),
        "the marker follows the verified statement at the ceremony's midpoint"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().repointed, 1);

    run_remove_old(&s.successor_engine, &s.channel, &s.nest, &s.alice_actor).await;
    let applied = poll_folder(&s, &mut cursor).await;
    assert_eq!(
        applied, 1,
        "the successor's remove-old is admitted by the re-stamped policy"
    );
    let members = s.bob.group_members(&s.channel);
    assert!(
        !members.contains(&s.alice_actor),
        "the retired leaf is gone"
    );
    assert!(members.contains(&s.successor));
}

/// Without the statement, the successor is a non-owner to bob's seat and its
/// remove-old is refused: the marker still names alice, and alice's leaf stays
/// — the policy the re-stamp exists to satisfy is really enforced here.
#[tokio::test]
async fn without_the_statement_the_successors_remove_old_is_refused_on_a_member_seat() {
    let s = folder_succession_setup();
    s.bob_backend
        .set_succession_witness(Arc::new(HeadWitness(s.head)));
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    run_remove_old(&s.successor_engine, &s.channel, &s.nest, &s.alice_actor).await;
    let applied = poll_folder(&s, &mut cursor).await;
    assert_eq!(applied, 1, "only the owner-authored add folds in");
    assert_eq!(s.bob.folder_channel_owner(&s.channel), Some(s.alice_actor));
    assert!(
        s.bob.group_members(&s.channel).contains(&s.alice_actor),
        "the refused remove-old left the predecessor seated on bob's view"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().repointed, 0);
}

/// A true statement is public and any member may carry one ahead of the
/// sweep: with no successor leaf here it re-stamps nothing (dropped, tallied),
/// so the old leaf's own add — its one constructive act — is not refused by a
/// marker that moved too early. When the ceremony reaches the channel, the
/// statement posted beside the add does the work.
#[tokio::test]
async fn a_statement_carried_ahead_of_the_add_re_stamps_nothing() {
    let s = folder_succession_setup();
    s.bob_backend
        .set_succession_witness(Arc::new(HeadWitness(s.head)));
    let mut cursor = 0;

    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    poll_folder(&s, &mut cursor).await;
    assert_eq!(s.bob.folder_channel_owner(&s.channel), Some(s.alice_actor));
    let counts = s.bob_backend.succession_statement_counts();
    assert_eq!(
        (counts.not_in_this_group, counts.repointed, counts.parked),
        (1, 0, 0)
    );

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    poll_folder(&s, &mut cursor).await;
    assert_eq!(s.bob.folder_channel_owner(&s.channel), Some(s.successor));
}

/// A statement the witness cannot settle yet — no anchor for alice held this
/// session — parks **by channel** (a folder channel has no thread), and the
/// harvest re-drive settles it once the anchor lands: the marker moves then,
/// not before.
#[tokio::test]
async fn a_witness_refused_statement_parks_by_channel_and_re_stamps_on_the_harvest_re_drive() {
    let s = folder_succession_setup();
    let witness = Arc::new(SwitchWitness::default());
    s.bob_backend.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    poll_folder(&s, &mut cursor).await;
    assert_eq!(
        s.bob.folder_channel_owner(&s.channel),
        Some(s.alice_actor),
        "refused for now: nothing moves"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().parked, 1);

    // The harvest seeds alice's anchor and re-drives what was parked on her.
    witness.arm(s.head);
    let manager = ConversationsManager::new();
    let repointed = redrive_parked_successions(&s.bob_backend, &manager, &s.alice_actor).await;
    assert_eq!(repointed, 1);
    assert_eq!(s.bob.folder_channel_owner(&s.channel), Some(s.successor));
}

/// A statement no key of alice's ever signed — what an in-group member forging
/// `old_actor_id`s posts: structurally perfect, refused by any witness that
/// holds alice's real head, and indistinguishable from the honest one to a
/// witness that holds none.
fn forged_folder_statement(
    old_actor_id: ActorId,
    new_actor_id: ActorId,
) -> fauna_core::recovery::SignedIdentitySuccession {
    let forged_recovery = fauna_core::recovery::RecoveryKey::generate();
    let forged_old = ActorKeypair::generate();
    let forged_new = ActorKeypair::generate();
    fauna_core::recovery::IdentitySuccession {
        old_actor_id,
        new_actor_id,
        recovery_pubkey: forged_recovery.public(),
        seq: 2,
        created_at: Timestamp::now(),
    }
    .sign(
        &forged_recovery,
        forged_new.signing_key(),
        Some(forged_old.signing_key()),
    )
    .expect("a forged statement still signs")
}

/// The refused-then-memoized fork, closed — the folder commit walk inherits
/// the harvest wait (`federation.md` § Cross-nest shared folders + channel
/// append → *The marker follows the owner's verified succession*, the
/// 2026-09-28 amendment). A member seat whose witness refuses the statement
/// at first (no anchor for alice yet) and verifies it after the harvest seeds
/// her: the successor's remove-old that follows the statement in the same
/// walk is HELD — the walk stops before it, cursor behind it, nothing
/// decrypted, nothing memoized — and folds in once the re-drive re-stamps the
/// marker. No `PolicyRefusedCommit`, no fork.
#[tokio::test]
async fn a_witness_refused_statement_holds_the_remove_old_until_the_harvest_settles_it() {
    let s = folder_succession_setup();
    let witness = Arc::new(SwitchWitness::default());
    s.bob_backend.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );
    // A sweep runs this session — the one condition under which the walk may
    // wait, because the sweep is what ends the wait.
    s.bob_backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    run_remove_old(&s.successor_engine, &s.channel, &s.nest, &s.alice_actor).await;

    let outcome = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(outcome.applied, 1, "the owner-authored add folds in");
    assert!(
        outcome.stalled,
        "the remove-old behind a parked statement is HELD, not refused"
    );
    assert_eq!(
        s.bob.folder_channel_owner(&s.channel),
        Some(s.alice_actor),
        "refused for now: the marker has not moved"
    );
    assert!(
        s.bob.group_members(&s.channel).contains(&s.alice_actor),
        "the held commit was not consumed"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().parked, 1);

    // The same walk again, still without an anchor: still held, never memoized
    // — the hold is idempotent across passes.
    let again = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!((again.applied, again.stalled), (0, true));

    // The harvest seeds alice's anchor and re-drives what was parked on her:
    // the marker moves, and the NEXT pass admits the remove-old through the
    // re-stamped policy.
    witness.arm(s.head);
    let manager = ConversationsManager::new();
    let repointed = redrive_parked_successions(&s.bob_backend, &manager, &s.alice_actor).await;
    assert_eq!(repointed, 1);
    assert_eq!(s.bob.folder_channel_owner(&s.channel), Some(s.successor));

    let released = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(
        (released.applied, released.stalled),
        (1, false),
        "the successor's remove-old folds in — it was never memoized"
    );
    let members = s.bob.group_members(&s.channel);
    assert!(
        !members.contains(&s.alice_actor),
        "the retired leaf is gone"
    );
    assert!(members.contains(&s.successor));
}

/// The session boundary no longer ends the hold — the at-rest folder park
/// (`federation.md` § Cross-nest shared folders + channel append → *The folder
/// commit walk inherits the harvest wait*). Bob's seat parks the refused
/// statement and holds the successor's remove-old, and its session ends before
/// the sweep has spoken for alice. The relaunch walks the log from 0 again and
/// cannot re-read the statement (its decrypt consumed the ratchet generation),
/// yet its first walk still holds the remove-old — the park rested with the
/// engine — and folds it once the sweep's settle re-stamps the marker. No
/// `PolicyRefusedCommit`, no fork.
#[tokio::test]
async fn the_hold_behind_a_parked_folder_statement_survives_a_relaunch() {
    let bob_secret = [33u8; 32];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let s = folder_succession_setup_with(Arc::new(
        MlsEngine::new(ActorKeypair::from_secret(bob_secret), tmp.path()).unwrap(),
    ));
    let witness = Arc::new(SwitchWitness::default());
    s.bob_backend.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );
    s.bob_backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    run_remove_old(&s.successor_engine, &s.channel, &s.nest, &s.alice_actor).await;
    let outcome = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!((outcome.applied, outcome.stalled), (1, true));

    // The session ends before the sweep has spoken for alice.
    let FolderSuccessionSetup {
        alice_actor,
        bob,
        bob_backend,
        channel,
        nest,
        successor,
        head,
        ..
    } = s;
    // A real session persists what it walked (the replica tick, a later
    // stamp); without it the relaunch would re-read the statement and prove
    // nothing about the park.
    bob.save_state()
        .expect("the session persisted what it walked");
    drop(bob_backend);
    drop(bob);

    // The relaunch: a fresh backend over the same engine store, a fresh sweep,
    // a folder cursor seeded to 0 as every launch seeds it.
    let bob = Arc::new(MlsEngine::new(ActorKeypair::from_secret(bob_secret), tmp.path()).unwrap());
    let backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob.identity_actor_id(),
    ));
    let witness = Arc::new(SwitchWitness::default());
    backend.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );
    backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;
    let held = poll_inbound_folder(&backend, &channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert!(
        held.stalled,
        "the relaunch's first walk still holds behind the rested statement"
    );
    assert_eq!(bob.folder_channel_owner(&channel), Some(alice_actor));
    assert!(
        bob.group_members(&channel).contains(&alice_actor),
        "the remove-old was neither applied nor refused"
    );

    // The sweep speaks for alice this session: the rested statement verifies,
    // the marker moves, and the next pass admits the remove-old.
    witness.arm(head);
    let manager = ConversationsManager::new();
    let repointed = redrive_parked_successions(&backend, &manager, &alice_actor).await;
    assert_eq!(repointed, 1);
    assert_eq!(bob.folder_channel_owner(&channel), Some(successor));
    let released = poll_inbound_folder(&backend, &channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert!(!released.stalled, "the walk reaches the head");
    let members = bob.group_members(&channel);
    assert!(
        !members.contains(&alice_actor),
        "the successor's remove-old folded in — it was never memoized"
    );
    assert!(members.contains(&successor));
    assert!(
        bob.parked_folder_succession(&channel).is_none(),
        "the settle forgot the rested copy"
    );
}

/// The at-rest park's bound: the sweep speaking for the owner forgets the
/// rested copy whatever it decided, so a forged statement costs at most one
/// hold window per launch — the launch after the settle holds nothing.
#[tokio::test]
async fn a_forged_statements_hold_does_not_recur_on_the_launch_after_the_settle() {
    let bob_secret = [44u8; 32];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let s = folder_succession_setup_with(Arc::new(
        MlsEngine::new(ActorKeypair::from_secret(bob_secret), tmp.path()).unwrap(),
    ));
    s.bob_backend
        .set_succession_witness(Arc::new(SwitchWitness::default()));
    s.bob_backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(
        &s.alice,
        &s.channel,
        &s.nest,
        &forged_folder_statement(s.alice_actor, s.successor),
    )
    .await;
    let outcome = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(outcome.applied, 1);
    assert!(
        s.bob.parked_folder_succession(&s.channel).is_some(),
        "the refused statement rests with the engine"
    );

    // The sweep settles alice without an anchor: the forgery re-points
    // nothing, and the rested copy is forgotten.
    let manager = ConversationsManager::new();
    let repointed = settle_parked_successions(&s.bob_backend, &manager, &s.alice_actor).await;
    assert_eq!(repointed, 0);
    assert!(s.bob.parked_folder_succession(&s.channel).is_none());

    // The relaunch, with an owner commit waiting on the log.
    let FolderSuccessionSetup {
        alice,
        alice_actor,
        bob,
        bob_backend,
        channel,
        nest,
        successor,
        ..
    } = s;
    bob.save_state()
        .expect("the session persisted what it walked");
    drop(bob_backend);
    drop(bob);
    let successor_leaf = alice
        .find_leaf_by_identity(&channel, &successor)
        .expect("the successor's leaf");
    let commit = alice
        .remove_member(&channel, successor_leaf)
        .expect("the owner's remove commit");
    nest.channel_send(
        channel.to_string(),
        ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .expect("the owner posts the commit");

    let bob = Arc::new(MlsEngine::new(ActorKeypair::from_secret(bob_secret), tmp.path()).unwrap());
    let backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob.identity_actor_id(),
    ));
    backend.set_succession_witness(Arc::new(SwitchWitness::default()));
    backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;
    let walked = poll_inbound_folder(&backend, &channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert!(!walked.stalled, "the forgery's hold did not come back");
    assert_eq!(bob.folder_channel_owner(&channel), Some(alice_actor));
    assert!(
        !bob.group_members(&channel).contains(&successor),
        "the owner's commit folded in"
    );
}

/// The bound on the hold: a forged statement naming the owner (any member may
/// post one, and to a witness holding no anchor it is indistinguishable from
/// the honest one) holds the channel's commit walk only until the sweep has
/// settled the owner — here without seeding anything — and never past it: the
/// walk resumes, the owner's commit folds, the marker never moved, and a
/// second forged statement after the settle holds nothing at all.
#[tokio::test]
async fn a_forged_statement_holds_the_walk_only_until_the_harvest_settles_the_owner() {
    let s = folder_succession_setup();
    let witness = Arc::new(SwitchWitness::default());
    s.bob_backend.set_succession_witness(
        Arc::clone(&witness) as Arc<dyn fauna_conversations::backend::SuccessionWitness>
    );
    s.bob_backend.arm_succession_harvest_wait().await;
    let mut cursor = 0;

    // An owner-authored add, the forgery, then an owner-authored commit the
    // forgery now stands in front of.
    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    let forged = forged_folder_statement(s.alice_actor, s.successor);
    post_statement(&s.alice, &s.channel, &s.nest, &forged).await;
    let successor_leaf = s
        .alice
        .find_leaf_by_identity(&s.channel, &s.successor)
        .expect("the successor's leaf");
    let commit = s
        .alice
        .remove_member(&s.channel, successor_leaf)
        .expect("the owner's remove commit");
    s.nest
        .channel_send(
            s.channel.to_string(),
            ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .expect("the owner posts the commit");

    let outcome = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(
        (outcome.applied, outcome.stalled),
        (1, true),
        "the forgery holds the walk while the sweep has not settled alice"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().parked, 1);

    // The sweep settles alice WITHOUT an anchor (nothing new, a refusal, a
    // spent budget): the statement stays refused, and the hold ENDS.
    let manager = ConversationsManager::new();
    let repointed = settle_parked_successions(&s.bob_backend, &manager, &s.alice_actor).await;
    assert_eq!(repointed, 0, "a forgery re-points nothing");
    let released = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(
        (released.applied, released.stalled),
        (1, false),
        "the owner's commit folds once the sweep has spoken for the owner"
    );
    assert_eq!(
        s.bob.folder_channel_owner(&s.channel),
        Some(s.alice_actor),
        "the marker never moved"
    );
    assert!(!s.bob.group_members(&s.channel).contains(&s.successor));

    // A second forgery after the settle holds nothing: the sweep has spoken
    // for alice this session, so nothing will change the verdict.
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    post_statement(
        &s.alice,
        &s.channel,
        &s.nest,
        &forged_folder_statement(s.alice_actor, carol.identity_actor_id()),
    )
    .await;
    run_add_successor(&s.alice, &carol, &s.channel, &s.nest).await;
    let unheld = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(
        (unheld.applied, unheld.stalled),
        (1, false),
        "after the settle the walk never waits on this owner again"
    );
}

/// The hold fails OPEN, exactly as the witness's own wait does: in a session
/// that runs no sweep nothing would ever end it, so the walk never waits — a
/// refused statement parks as before and the commit behind it meets the
/// marker as it stands (the declared residual, unchanged for that session).
#[tokio::test]
async fn without_an_armed_sweep_the_folder_walk_never_holds() {
    let s = folder_succession_setup();
    s.bob_backend
        .set_succession_witness(Arc::new(SwitchWitness::default()));
    let mut cursor = 0;

    run_add_successor(&s.alice, &s.successor_engine, &s.channel, &s.nest).await;
    post_statement(&s.alice, &s.channel, &s.nest, &s.signed).await;
    run_remove_old(&s.successor_engine, &s.channel, &s.nest, &s.alice_actor).await;

    let outcome = poll_inbound_folder(&s.bob_backend, &s.channel, &mut cursor, 50)
        .await
        .expect("the folder poll walks");
    assert_eq!(
        (outcome.applied, outcome.stalled),
        (1, false),
        "no sweep, no wait: the walk reaches the head"
    );
    assert_eq!(s.bob_backend.succession_statement_counts().parked, 1);
}

/// What makes the settle a guaranteed event: every folder channel's recorded
/// owner joins the peer-anchor harvest's walk — the sweep walked thread
/// rosters and room-policy names only, and a folder channel has no thread, so
/// an owner the member shares no conversation with was never harvested and
/// nothing would ever end the hold. The seat's own identity is never offered.
#[tokio::test]
async fn the_folder_owner_joins_the_harvest_walk() {
    let s = folder_succession_setup();
    let manager = ConversationsManager::new();
    manager.register_backend(s.bob_backend.clone());
    let bob_actor = s.bob.identity_actor_id();
    // A set bob owns himself: the marker names him, and he is nobody's peer.
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let (own_set, _) = s
        .bob
        .create_group(&carol.generate_key_packages(1).unwrap())
        .expect("bob mints his own set");
    s.bob.mark_folder_channel_owner(&own_set, &bob_actor);

    let walk = manager.harvest_walk_actors();
    assert!(
        walk.contains(&s.alice_actor),
        "the recorded owner of a joined set is harvested"
    );
    assert!(
        !walk.contains(&bob_actor),
        "the seat's own identity is never a peer to harvest"
    );
}

/// 5d(d) — the remaining member's **epoch-advance liveness**
/// (`mls-group-key-material.md` § Rotate-on-removal). When the owner removes a
/// member, `FoldersAuthor::drive_removal` posts the MLS Remove commit to the
/// set's channel; a REMAINING member's folder commit poll
/// ([`poll_inbound_folder`]) applies it, advancing their engine to the
/// post-removal epoch so the re-published content-key envelope opens — while
/// the REMOVED member stays excluded. Without the poll, the remaining member is
/// stuck at the old epoch and fails closed on the new envelope (the pre-poll
/// assertion below is exactly that former gap).
#[tokio::test]
async fn folder_commit_poll_advances_remaining_member_to_post_removal_epoch() {
    // A three-party set: alice owns, bob will be removed, carol remains.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps: Vec<_> = bob
        .generate_key_packages(1)
        .unwrap()
        .into_iter()
        .chain(carol.generate_key_packages(1).unwrap())
        .collect();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    assert_eq!(bob.join_from_welcome_bytes(&welcome_bytes).unwrap(), ch);
    assert_eq!(carol.join_from_welcome_bytes(&welcome_bytes).unwrap(), ch);

    let nest = Arc::new(MockNest::default());
    let carol_backend = FaunaMlsBackend::new(
        carol.clone(),
        nest.clone(),
        "carol",
        carol.identity_actor_id(),
    );

    // The poll set is DERIVED from the engine (no thread bound, not scheduling),
    // so it holds even with the in-memory folder marker empty — the restart
    // case (`mark_folder_channel` does not survive a relaunch).
    assert_eq!(
        carol_backend.folder_poll_channels(),
        vec![ch],
        "the joined set is discovered from the engine's persisted groups"
    );

    // gen-1 published pre-removal — carol, at the shared epoch, opens it.
    let gen1 = fauna_core::folder_keys::FolderContentKeys::genesis([1u8; 32], 1_000);
    let sealed1 = alice.seal_content_key_envelope(&ch, &gen1, None).unwrap();
    assert_eq!(
        carol
            .open_content_key_envelope(&ch, &sealed1.sealed)
            .unwrap(),
        gen1
    );

    // Owner removes bob and — as `drive_removal` now does — posts the Remove
    // commit to the set's channel, then re-publishes the rotated bundle under
    // the post-removal epoch.
    let bob_leaf = alice
        .find_leaf_by_identity(&ch, &bob.identity_actor_id())
        .expect("bob's leaf");
    let commit = alice.remove_member(&ch, bob_leaf).expect("remove commit");
    let envelope = ChannelEnvelope::Commit(commit).to_bytes().unwrap();
    nest.channel_send(ch.to_string(), envelope, None, vec![])
        .await
        .expect("owner posts the commit");
    let mut gen2 = gen1.clone();
    gen2.rotate([2u8; 32], 2_000);
    let sealed2 = alice.seal_content_key_envelope(&ch, &gen2, None).unwrap();

    // Pre-poll: carol is stuck at the pre-removal epoch — fail-closed on the
    // re-published envelope (the 5d(d) liveness gap this poll closes).
    assert!(
        carol
            .open_content_key_envelope(&ch, &sealed2.sealed)
            .is_err(),
        "an un-advanced member cannot open the post-removal envelope"
    );

    // The folder commit poll applies the owner's Remove commit.
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll");
    assert_eq!(outcome.applied, 1, "one commit advanced carol's epoch");
    assert!(!outcome.stalled, "a clean walk reaches the head");

    // Carol reads the re-published envelope — full history (gen 1 + gen 2).
    let opened = carol
        .open_content_key_envelope(&ch, &sealed2.sealed)
        .expect("post-poll open");
    assert_eq!(opened, gen2);

    // A restart-fresh cursor re-walks the log harmlessly (past-epoch quiet skip).
    let mut fresh = 0i64;
    assert_eq!(
        poll_inbound_folder(&carol_backend, &ch, &mut fresh, 0)
            .await
            .unwrap()
            .applied,
        0,
        "re-applied commit is a quiet skip"
    );

    // The REMOVED member polls the same log, processes its own removal, and
    // still cannot read the post-removal envelope (rotate-on-removal holds).
    let bob_backend =
        FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob.identity_actor_id());
    let mut bob_cursor = 0i64;
    let _ = poll_inbound_folder(&bob_backend, &ch, &mut bob_cursor, 0).await;
    assert!(
        bob.open_content_key_envelope(&ch, &sealed2.sealed).is_err(),
        "the removed member stays excluded (forward secrecy)"
    );
}

// ── Member content-key custody ingest (Phase 0 — the read leg) ──────────────
//
// The M2 model holds each member's copy of the content-key bundle in their own
// folder-key custody; the session-level ingest driver
// (`FaunaMlsBackend::maybe_ingest_folder_custody`, driven at join + on each
// rotation-commit receipt) is what populates it, so a *member* (not just the
// owner) can decrypt a shared set's content. A registered `FolderCustodySink`
// supplies the fetch (over `content_key.get`) + the persist (to the folder-key store); the
// open runs in the session's engine. Here a mock serves a pre-sealed envelope and
// records the bundles the driver merged.

/// Records the content-key bundles the ingest driver merges, and serves the
/// owner's latest sealed envelope for the driver to open — the test double for
/// `NestFolderCustodySink`.
struct MockFolderCustodySink {
    /// The sealed envelope `fetch_sealed_envelope` returns (the owner's latest
    /// publish), or `None` to simulate `not_published`.
    sealed: Mutex<Option<fauna_conversations::backend::FetchedEnvelope>>,
    /// Every bundle the driver merged, in order — proves the fetch → open → merge
    /// chain reached custody with the right generations.
    received: Mutex<Vec<fauna_core::folder_keys::FolderContentKeys>>,
    /// The driver's `may_move` verdict on each merge's signer.
    moves: Mutex<Vec<bool>>,
    /// What `refetch_owed` answers next — spent by the asking, as the real
    /// sink's answer is.
    refetch_owed: Mutex<bool>,
}

impl MockFolderCustodySink {
    fn new() -> Self {
        Self {
            sealed: Mutex::new(None),
            received: Mutex::new(Vec::new()),
            moves: Mutex::new(Vec::new()),
            refetch_owed: Mutex::new(false),
        }
    }
    /// The sink's throttled check found the fetch re-owed (the nest's nonce
    /// echo moved, or the engine's host refused a row `signature_invalid`).
    fn owe_refetch(&self) {
        *self.refetch_owed.lock().unwrap() = true;
    }
    /// The owner's latest publish, as the sink hands it on once its signature
    /// verified: who signed it, and the sealed bytes.
    fn set_sealed(&self, signer: ActorId, bytes: Vec<u8>) {
        *self.sealed.lock().unwrap() = Some(fauna_conversations::backend::FetchedEnvelope {
            signer,
            sealed: bytes,
        });
    }
    fn received(&self) -> Vec<fauna_core::folder_keys::FolderContentKeys> {
        self.received.lock().unwrap().clone()
    }
    fn moves(&self) -> Vec<bool> {
        self.moves.lock().unwrap().clone()
    }
}

#[async_trait]
impl FolderCustodySink for MockFolderCustodySink {
    async fn fetch_sealed_envelope(
        &self,
        _channel_id_hex: &str,
    ) -> Option<fauna_conversations::backend::FetchedEnvelope> {
        self.sealed.lock().unwrap().clone()
    }
    async fn merge_and_persist(
        &self,
        _channel_id: &[u8; 32],
        payload: fauna_core::folder_keys::ContentKeyEnvelopePayload,
        may_move: bool,
    ) -> bool {
        self.received.lock().unwrap().push(payload.keys);
        self.moves.lock().unwrap().push(may_move);
        true
    }
    async fn refetch_owed(&self, _channel_id: &[u8; 32]) -> bool {
        std::mem::take(&mut *self.refetch_owed.lock().unwrap())
    }
}

/// Phase 0 — the member custody-ingest read leg. The session-level driver must,
/// through a registered `FolderCustodySink`: (a) at JOIN, fetch the owner's
/// sealed content-key envelope, open it at the just-joined group epoch, and merge
/// the generations into the member's own custody; (b) on a ROTATION-commit
/// receipt, re-fetch + merge the post-removal bundle after the poll advances the
/// epoch. Without this the member joins but can never decrypt the set's content
/// (the ratified-and-unbuilt gap, `folders.md` § Sharing).
#[tokio::test]
async fn member_custody_ingest_fires_at_join_and_after_a_rotation_commit() {
    // alice owns; bob will be removed; carol is the ingesting member.
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps: Vec<_> = bob
        .generate_key_packages(1)
        .unwrap()
        .into_iter()
        .chain(carol.generate_key_packages(1).unwrap())
        .collect();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    assert_eq!(bob.join_from_welcome_bytes(&welcome_bytes).unwrap(), ch);

    let nest = Arc::new(MockNest::default());
    let carol_backend = FaunaMlsBackend::new(
        carol.clone(),
        nest.clone(),
        "carol",
        carol.identity_actor_id(),
    );
    let sink = Arc::new(MockFolderCustodySink::new());
    carol_backend.set_folder_custody_sink(sink.clone());

    // The owner publishes gen-1, sealed under the pre-removal epoch carol joins
    // at; the mock serves it to the driver.
    let gen1 = fauna_core::folder_keys::FolderContentKeys::genesis([1u8; 32], 1_000);
    sink.set_sealed(
        alice.identity_actor_id(),
        alice
            .seal_content_key_envelope(&ch, &gen1, None)
            .unwrap()
            .sealed,
    );

    // (a) JOIN through the shared folder join path → the driver ingests gen-1.
    join_folder_welcome(
        &carol_backend,
        &ch.to_string(),
        &welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("carol joins the shared set");
    assert_eq!(
        sink.received(),
        vec![gen1.clone()],
        "join ingests the owner's current envelope into member custody"
    );

    // Owner removes bob, posts the Remove commit to the set channel, and
    // re-publishes gen-2 sealed under the POST-removal epoch.
    let bob_leaf = alice
        .find_leaf_by_identity(&ch, &bob.identity_actor_id())
        .expect("bob's leaf");
    let commit = alice.remove_member(&ch, bob_leaf).expect("remove commit");
    nest.channel_send(
        ch.to_string(),
        ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .expect("owner posts the commit");
    let mut gen2 = gen1.clone();
    gen2.rotate([2u8; 32], 2_000);
    sink.set_sealed(
        alice.identity_actor_id(),
        alice
            .seal_content_key_envelope(&ch, &gen2, None)
            .unwrap()
            .sealed,
    );

    // (b) The rotation-commit poll advances carol's epoch AND re-ingests: the
    // driver fetches the re-published bundle and merges gen-2 (only possible
    // because the poll advanced her past the removal — the D4 ordering).
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll");
    assert_eq!(outcome.applied, 1, "the rotation commit advanced carol");
    assert_eq!(
        sink.received(),
        vec![gen1, gen2],
        "the rotation poll re-ingests the new generation into custody"
    );
    assert_eq!(
        sink.moves(),
        vec![true, true],
        "the owner the Welcome recorded may move what the member holds"
    );
}

/// `writer-signed-change-records.md` ruling (11)(b): only the owner this
/// member's MLS state records may move its nonce — an envelope another
/// identity signed (a member with a nest's help, a predecessor's stolen seed)
/// is handed to custody as confirm-only.
#[tokio::test]
async fn member_custody_ingest_moves_nothing_under_a_non_owners_signature() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps = carol.generate_key_packages(1).unwrap();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    let nest = Arc::new(MockNest::default());
    let carol_backend = FaunaMlsBackend::new(
        carol.clone(),
        nest.clone(),
        "carol",
        carol.identity_actor_id(),
    );
    let sink = Arc::new(MockFolderCustodySink::new());
    carol_backend.set_folder_custody_sink(sink.clone());
    let gen1 = fauna_core::folder_keys::FolderContentKeys::genesis([1u8; 32], 1_000);
    sink.set_sealed(
        ActorKeypair::generate().actor_id(),
        alice
            .seal_content_key_envelope(&ch, &gen1, Some([0x5E; 32]))
            .unwrap()
            .sealed,
    );
    join_folder_welcome(
        &carol_backend,
        &ch.to_string(),
        &welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("carol joins the shared set");
    assert_eq!(
        sink.moves(),
        vec![false],
        "a stranger's signature moves nothing"
    );
}

/// A quiet folder poll (no commit applied) does NOT re-fetch once custody is
/// ingested — the D2 retry gate is on custody-absence, not every tick — but a
/// member whose first ingest failed (owner had not published) retries on the next
/// pass. Proves the network is not hit on every background poll while the
/// not-yet-ingested member still converges.
#[tokio::test]
async fn member_custody_ingest_retries_only_while_absent_then_settles() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps = carol.generate_key_packages(1).unwrap();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();
    assert_eq!(carol.join_from_welcome_bytes(&welcome_bytes).unwrap(), ch);

    let nest = Arc::new(MockNest::default());
    let carol_backend = FaunaMlsBackend::new(
        carol.clone(),
        nest.clone(),
        "carol",
        carol.identity_actor_id(),
    );
    let sink = Arc::new(MockFolderCustodySink::new());
    carol_backend.set_folder_custody_sink(sink.clone());

    // First poll: the owner has NOT published yet (mock returns `None`) — the
    // driver fetches, gets nothing, and does not mark the channel ingested.
    let mut cursor = 0i64;
    poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll 1");
    assert!(
        sink.received().is_empty(),
        "nothing published yet — nothing ingested"
    );

    // Owner publishes; the next quiet poll (still no commit) RE-fetches because
    // custody is still absent (the D2 retry), and now ingests.
    let gen1 = fauna_core::folder_keys::FolderContentKeys::genesis([1u8; 32], 1_000);
    sink.set_sealed(
        alice.identity_actor_id(),
        alice
            .seal_content_key_envelope(&ch, &gen1, None)
            .unwrap()
            .sealed,
    );
    poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll 2");
    assert_eq!(
        sink.received(),
        vec![gen1.clone()],
        "retry ingests once published"
    );

    // A third quiet poll does NOT re-fetch — custody is present and no epoch
    // advanced, so the driver skips the network (received unchanged).
    poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll 3");
    assert_eq!(
        sink.received(),
        vec![gen1],
        "a quiet poll after ingest does not re-fetch (D2 gate on absence)"
    );
}

/// `writer-signed-change-records.md` ruling (11)(b): a member re-fetches the
/// envelope also when the set's pushed nonce changes or a row of the set is
/// refused `signature_invalid` — neither of which advances the epoch. The sink
/// says so (`refetch_owed`), and the driver's next QUIET poll fetches once: the
/// owner's re-mint reaches the member with no commit on the channel, and the
/// poll after it is quiet again (no fetch storm).
#[tokio::test]
async fn member_custody_ingest_refetches_on_a_quiet_poll_when_the_sink_owes_it() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps = carol.generate_key_packages(1).unwrap();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    let welcome_bytes = welcome.to_bytes().unwrap();

    let nest = Arc::new(MockNest::default());
    let carol_backend = FaunaMlsBackend::new(
        carol.clone(),
        nest.clone(),
        "carol",
        carol.identity_actor_id(),
    );
    let sink = Arc::new(MockFolderCustodySink::new());
    carol_backend.set_folder_custody_sink(sink.clone());
    let keys = fauna_core::folder_keys::FolderContentKeys::genesis([1u8; 32], 1_000);
    let seal = |nonce: [u8; 32]| {
        alice
            .seal_content_key_envelope(&ch, &keys, Some(nonce))
            .unwrap()
            .sealed
    };
    sink.set_sealed(alice.identity_actor_id(), seal([0x01; 32]));
    join_folder_welcome(
        &carol_backend,
        &ch.to_string(),
        &welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("carol joins the shared set");
    assert_eq!(sink.received().len(), 1, "the join ingest");

    // The owner re-mints the set's nonce and re-publishes at the SAME epoch —
    // no commit reaches the channel. A quiet poll alone does not fetch.
    sink.set_sealed(alice.identity_actor_id(), seal([0x02; 32]));
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("quiet poll");
    assert_eq!(outcome.applied, 0, "no epoch advance");
    assert_eq!(sink.received().len(), 1, "a quiet poll does not fetch");

    // The sink's check finds the fetch re-owed: the next quiet poll fetches.
    sink.owe_refetch();
    poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("quiet poll, re-fetch owed");
    assert_eq!(
        sink.received().len(),
        2,
        "the member re-fetches the re-published envelope with no epoch advance"
    );

    // Spent by the attempt: the poll after it is quiet again.
    poll_inbound_folder(&carol_backend, &ch, &mut cursor, 0)
        .await
        .expect("quiet poll");
    assert_eq!(sink.received().len(), 2, "one fetch per owed signal");
}

/// Rule-2 "round toward the safe side" on the folder poll cursor
/// (`devices.md` § Cross-device MLS group-state sync): a logged commit the
/// local group has NOT incorporated and this pass cannot heal — here, the
/// device's own un-merged commit met with no `CommitGate` injected — must stop
/// the walk with `stalled = true`, leaving the cursor BEFORE that record.
/// Advancing past it would let a later gated send's `expect_no_commit_since`
/// baseline sit past an unincorporated epoch transition and fork the group.
/// Records after the stall stay unconsumed (they are future-epoch-unprocessable
/// anyway); the next pass retries from the same place.
#[tokio::test]
async fn folder_poll_stalls_before_unincorporated_own_leaf_commit() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let kps = bob.generate_key_packages(1).unwrap();
    let (ch, welcome) = alice.create_group(&kps).unwrap();
    bob.join_from_welcome_bytes(&welcome.to_bytes().unwrap())
        .unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_backend = FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice.identity_actor_id(),
    );
    alice_backend.mark_folder_channel(ch);

    // Alice's own commit lands on the log, but her engine no longer stages it
    // (the crash shape: the pending was lost / cleared before the merge). MLS
    // can never process an own-leaf commit, and with no gate there is no
    // resync — the walk must stall, not consume.
    let commit = alice.self_update(&ch).expect("stage own commit");
    alice.clear_pending_commit(&ch).expect("lose the pending");
    let envelope = ChannelEnvelope::Commit(commit).to_bytes().unwrap();
    nest.channel_send(ch.to_string(), envelope, None, vec![])
        .await
        .expect("own commit sits on the log");

    let epoch_before = alice.current_epoch(&ch).unwrap();
    let mut cursor = 0i64;
    let outcome = poll_inbound_folder(&alice_backend, &ch, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(outcome.stalled, "the walk reports the stall");
    assert_eq!(outcome.applied, 0);
    assert_eq!(
        cursor, 0,
        "the cursor stays BEFORE the unincorporated commit (Rule 2)"
    );
    assert_eq!(
        alice.current_epoch(&ch).unwrap(),
        epoch_before,
        "nothing was force-merged"
    );
}

/// The **chat-rail twin** of
/// [`folder_poll_stalls_before_unincorporated_own_leaf_commit`] — Rule-2 "round
/// toward the safe side" on the *conversation* poll cursor (`devices.md` §
/// Cross-device MLS group-state sync). This asymmetry (the folder rail pinned,
/// the chat rail not) is why the chat rail carried a silent fork:
/// `poll_inbound_conv` advanced its cursor past an own-leaf commit the local group
/// had never incorporated, so a later gated `remove_participant` took its
/// `expect_no_commit_since` baseline from *past* that epoch transition. The nest
/// accepted the rebuilt commit, every other member quiet-skipped it as
/// `PastEpochCommit`, and the sender merged and reported success — while the
/// "removed" member stayed in the live group and kept decrypting traffic.
///
/// The walk must stop **before** the unincorporated record and report
/// `stalled = true`, leaving both the commit and everything after it unconsumed
/// (they are future-epoch-unprocessable anyway); the next pass retries the heal.
#[tokio::test]
async fn conv_poll_stalls_before_unincorporated_own_leaf_commit() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();
    let channel_hex = channel_id.to_string();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice_actor,
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    // seq 1 — alice's OWN commit lands on the log, but her engine no longer
    // stages it: the crash shape (pending lost before the merge), and equally the
    // sibling-device shape, since the gate-less membership path merges without
    // ever CAS-putting the provider replica. MLS can never process an own-leaf
    // commit and NO `CommitGate` is injected here, so there is no resync arm —
    // `apply_inbound_commit` reports `Stalled { future_epoch: false }`.
    let commit = alice.self_update(&channel_id).expect("stage own commit");
    alice
        .clear_pending_commit(&channel_id)
        .expect("lose the pending");
    let envelope = ChannelEnvelope::Commit(commit.clone()).to_bytes().unwrap();
    nest.channel_send(channel_hex.clone(), envelope, None, vec![])
        .await
        .expect("own commit sits on the log");

    // seq 2 — bob processes that commit and speaks at the NEW epoch. Alice never
    // incorporated the transition, so this record is not hers to consume; a
    // cursor that ran past seq 1 would swallow it too.
    bob.process_commit(&channel_id, &commit)
        .expect("bob advances past alice's commit");
    let ct = bob
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: bob.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("past the stall".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    let epoch_before = alice.current_epoch(&channel_id).unwrap();
    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(outcome.stalled, "the walk reports the stall");
    assert_eq!(outcome.ingested, 0, "nothing past the stall is consumed");
    assert_eq!(
        cursor, 0,
        "the cursor stays BEFORE the unincorporated commit (Rule 2) — advancing past it is \
         what let a gated send rebase onto an epoch the group had already left"
    );
    assert_eq!(
        alice.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "nothing was force-merged"
    );
    assert!(
        manager
            .thread_detail(thread_id)
            .expect("thread")
            .messages
            .is_empty(),
        "the post-stall message stays unconsumed for the next pass"
    );
}

/// The DoS direction of the `apply_inbound_commit` taxonomy (Rule 2's *skip*
/// side, `devices.md` § Cross-device MLS group-state sync): an **intrinsically
/// invalid** commit — bytes no member can ever apply, postable by any in-group
/// member — must be `Skipped`, not `Stalled`. Under the naive
/// "every failed commit stalls" rounding, one garbage record would pin every
/// other member's cursor before it forever, permanently wedging the channel.
/// The group's canonical state never advanced past the junk either, so later
/// records still decrypt at the current epoch and nothing is lost by walking on.
#[tokio::test]
async fn conv_poll_skips_intrinsically_invalid_commit_without_wedging() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();
    let channel_hex = channel_id.to_string();

    let nest = Arc::new(MockNest::default());
    let backend = Arc::new(FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    // seq 1 — a garbage commit: decodable as a `ChannelEnvelope::Commit` but
    // junk to every member's MLS engine (any in-group member can post one).
    let junk = ChannelEnvelope::Commit(b"not an mls message at all".to_vec())
        .to_bytes()
        .unwrap();
    nest.channel_send(channel_hex.clone(), junk, None, vec![])
        .await
        .expect("junk commit sits on the log");

    // seq 2 — bob speaks at the (unchanged) current epoch.
    let ct = bob
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: bob.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("still flowing".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(!outcome.stalled, "junk must not wedge the channel");
    assert_eq!(outcome.ingested, 1, "the message past the junk is consumed");
    assert_eq!(cursor, 2, "the cursor advances past the invalid record");
    assert_eq!(
        manager
            .thread_detail(thread_id)
            .expect("thread")
            .messages
            .len(),
        1,
        "bob's message landed"
    );
}

/// The loss direction of the `apply_inbound_commit` taxonomy (Rule 2's *stall*
/// side): a commit that fails **locally** — here the earliest local condition,
/// the engine not holding the group yet (its Welcome still in flight) — must
/// `Stall` the walk, not be `Skipped`. Under the old catch-all-`Skipped`
/// rounding the cursor consumed the epoch transition it never applied, and
/// every later message (sealed under the new epoch) hit the decrypt-`continue`
/// and silently vanished — user-irrecoverable, since a sender cannot
/// MLS-decrypt its own application messages and MLS never re-seals an old
/// epoch. The stall is *healable*: once the Welcome lands, the same records
/// replay and everything flows — nothing was lost.
#[tokio::test]
async fn conv_poll_stalls_on_locally_unappliable_commit_and_heals_after_welcome() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let channel_hex = channel_id.to_string();

    // Alice advances the epoch and speaks at it, both on the log — while bob's
    // Welcome is still in flight (his engine has no group for the channel).
    let commit = alice.self_update(&channel_id).expect("stage own commit");
    alice
        .merge_pending_commit(&channel_id)
        .expect("alice merges her commit");
    let nest = Arc::new(MockNest::default());
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .expect("commit sits on the log");
    let ct = alice
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: alice.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("sealed under the new epoch".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    let backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "alice",
        fauna_addr("alice", alice.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");
    assert!(
        outcome.stalled,
        "a locally-unappliable commit stalls the walk"
    );
    assert_eq!(outcome.ingested, 0);
    assert_eq!(
        cursor, 0,
        "the cursor stays BEFORE the unincorporated commit (Rule 2)"
    );

    // The Welcome lands (the heal). The next pass replays the same records:
    // the commit now applies and the message past it decrypts — proving the
    // stall preserved, not lost, everything behind it.
    bob.join_from_welcome(welcome).unwrap();
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("re-poll");
    assert!(!outcome.stalled, "healed");
    assert_eq!(outcome.ingested, 1, "the once-stalled message survives");
    assert_eq!(cursor, 2);
    assert_eq!(
        manager
            .thread_detail(thread_id)
            .expect("thread")
            .messages
            .len(),
        1,
        "alice's message landed after the heal — nothing was dropped"
    );
}

/// The pin, driven through the production rail:
/// a member whose own private epoch decryption
/// keypairs are absent (torn snapshot / replica restore / storage read error)
/// meets a genuine membership commit the rest of the group applies cleanly.
/// The stage failure is *local* — the bytes are fine — so the poll must STALL
/// the cursor before the commit (Rule 2), never skip it: skipping consumes a
/// transition the group applied, and every later message sealed under the new
/// epoch hits the bare decrypt-`continue` and is silently dropped,
/// user-irrecoverable. The second poll drives the consumption-window memo:
/// the first pass consumed the sender-ratchet generation, so only the memo
/// keeps the retry classified local instead of "intrinsically invalid".
#[tokio::test]
async fn conv_poll_stalls_when_own_epoch_keys_are_absent_instead_of_eating_the_commit() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let charlie = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let channel_hex = channel_id.to_string();
    bob.join_from_welcome(welcome).unwrap();

    // Alice adds Charlie — a real update-path commit every intact member
    // applies — and speaks under the new epoch, both on the log.
    let charlie_kps = charlie.generate_key_packages(1).unwrap();
    let (commit, welcome_c) = alice.add_member(&channel_id, &charlie_kps[0]).unwrap();
    charlie.join_from_welcome(welcome_c).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .expect("commit sits on the log");
    let ct = alice
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: alice.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("sealed under the epoch bob missed".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // The only delta on Bob: his own private epoch decryption keypairs are
    // gone. The commit bytes and the group's shared public tree are intact.
    bob.delete_own_epoch_keypairs_for_test(&channel_id).unwrap();

    let backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "alice",
        fauna_addr("alice", alice.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");
    assert!(
        outcome.stalled,
        "a local key absence stalls the walk — it must never classify as an \
         intrinsically invalid record the cursor may eat"
    );
    assert_eq!(outcome.ingested, 0);
    assert_eq!(
        cursor, 0,
        "the cursor stays BEFORE the unincorporated commit (Rule 2)"
    );

    // Pass 2 — the consumption-window memo leg: pass 1's decrypt consumed the
    // sender-ratchet generation, so without the memo this retry would
    // re-decrypt, fail on the consumed generation, misclassify as
    // intrinsically invalid, and the cursor would advance past the commit —
    // silently dropping Alice's message. It must stall again instead.
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("re-poll");
    assert!(
        outcome.stalled,
        "the retry stays a stall (memo) — the loss must not re-open one pass later"
    );
    assert_eq!(outcome.ingested, 0);
    assert_eq!(cursor, 0, "the cursor never ate the commit");
    assert_eq!(
        manager
            .thread_detail(thread_id)
            .expect("thread")
            .messages
            .len(),
        0,
        "nothing was silently half-ingested"
    );
}

/// The pin: the
/// background folder sweep derives its channel set from the engine's groups
/// minus the RAM-only bindings, so after a relaunch an **unbound chat
/// channel** used to be swept as folder — and that sweep applies membership
/// commits while skipping application messages, advancing the shared engine's
/// epoch past an unread chat message, which MLS forward secrecy makes
/// permanently undecryptable (silent chat loss). The durable chat marker
/// (`bind_channel` → provider KV, riding the same snapshot/replica as the
/// group) must keep the channel out of the sweep across the relaunch, so the
/// later re-bound conv poll replays the log **in order** and loses nothing.
#[tokio::test]
async fn folder_sweep_never_eats_a_commit_on_an_unbound_chat_channel() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    let channel_hex = channel_id.to_string();
    bob.join_from_welcome(welcome).unwrap();
    let epoch_joined = bob.current_epoch(&channel_id).unwrap();
    let nest = Arc::new(MockNest::default());

    // Run 1: bob's app binds the thread — stamping the durable chat marker.
    {
        let backend1 = Arc::new(FaunaMlsBackend::new(
            bob.clone(),
            nest.clone(),
            "bob",
            bob.identity_actor_id(),
        ));
        backend1.bind_channel(ThreadId("t-first-launch".into()), channel_id);
    } // backend1 drops — the "relaunch" empties every RAM-only binding.

    // While bob is between runs, alice interleaves [app@epoch0, commit,
    // app@epoch1] on the log — the reviewer's exact shape.
    let msg = |seq: u64, body: &str| ChannelMessage {
        sender: alice.identity_actor_id(),
        sequence: seq,
        channel_epoch: 0,
        body: ChannelMessageBody::Text(body.into()),
        timestamp: Timestamp::now(),
    };
    let ct0 = alice
        .encrypt(&channel_id, &msg(1, "sealed at epoch 0"))
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct0).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();
    let commit = alice.self_update(&channel_id).expect("stage");
    alice.merge_pending_commit(&channel_id).expect("merge");
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Commit(commit).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();
    let ct1 = alice
        .encrypt(&channel_id, &msg(2, "sealed at epoch 1"))
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct1).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // Run 2: fresh backend over the same (persisted) engine — the chat thread
    // is not yet re-bound. The background folder sweep must NOT sweep the
    // chat channel: the durable marker classifies it even while unbound.
    let backend2 = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob.identity_actor_id(),
    ));
    assert!(
        !backend2.is_folder_rail(&channel_id),
        "an unbound chat channel must not classify as folder rail"
    );
    for ch in backend2.folder_poll_channels() {
        let mut fs_cursor = 0i64;
        poll_inbound_folder(&backend2, &ch, &mut fs_cursor, 0)
            .await
            .expect("sweep");
    }
    assert_eq!(
        bob.current_epoch(&channel_id).unwrap(),
        epoch_joined,
        "the sweep must not apply the chat channel's commit past the unread \
         earlier-epoch message (MLS forward secrecy would make it permanently \
         undecryptable)"
    );

    // The thread re-binds (its history slice arrives) and the conv poll
    // replays the log in seq order — nothing was lost.
    let manager = ConversationsManager::new();
    manager.register_backend(backend2.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "alice",
        fauna_addr("alice", alice.identity_actor_id()),
    ));
    backend2.bind_channel(thread_id.clone(), channel_id);
    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend2, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("conv poll");
    assert!(!outcome.stalled);
    assert_eq!(
        outcome.ingested, 2,
        "both messages land — the epoch-0 one would have been silently \
         dropped had the sweep advanced the epoch first"
    );
}

/// Gate-less crash reconcile, arm 1 (`devices.md` § Durability rules Rule 1 —
/// the durable local pending): the client staged a membership commit, persisted
/// the provider snapshot, the nest **accepted the send**, and the process died
/// before the merge — the exact accept→merge crash window that used to strand a
/// permanently gate-less device with "a detector but no heal path at all". On
/// relaunch the staged pending and its blake3 stamp reload from the snapshot;
/// the poll meets the own-leaf commit on the log, proves by identity that the
/// reloaded pending IS that commit, and merges it — converging the device with
/// no gate, no replica plane, and no user action. Messages sealed under the new
/// epoch by other members then decrypt normally (nothing lost).
#[tokio::test]
async fn gate_less_crash_after_accept_heals_by_merging_the_reloaded_pending() {
    let secret = [7u8; 32];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([8u8; 32])).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let nest = Arc::new(MockNest::default());

    // Run 1 — the crashing process: create the group, stage a self-update,
    // persist (the else-branch's stage→persist step), get the send accepted,
    // and die before the merge.
    let (channel_id, channel_hex) = {
        let alice1 = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        let (channel_id, welcome) = alice1.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let commit = alice1.self_update(&channel_id).expect("stage");
        alice1.save_state().expect("persist the staged pending");
        let envelope = ChannelEnvelope::Commit(commit.clone()).to_bytes().unwrap();
        nest.channel_send(channel_id.to_string(), envelope, None, vec![])
            .await
            .expect("the nest accepted the send");
        // Bob converges on the landed commit and speaks at the new epoch.
        bob.process_commit(&channel_id, &commit).unwrap();
        (channel_id, channel_id.to_string())
        // alice1 drops here without merging — the crash.
    };
    let ct = bob
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: bob.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("new-epoch traffic".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // Run 2 — the relaunch: the pending and its stamp reload as *resumed*.
    let alice2 = Arc::new(MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap());
    assert!(alice2.has_pending_commit(&channel_id), "pending reloads");
    assert!(alice2.has_resumed_pending(&channel_id), "and is resumed");
    let epoch_before = alice2.current_epoch(&channel_id).unwrap();

    let backend = Arc::new(FaunaMlsBackend::new(
        alice2.clone(),
        nest.clone(),
        "alice",
        alice2.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(
        !outcome.stalled,
        "the identity-matched pending merges — no stall"
    );
    assert_eq!(outcome.ingested, 1, "the new-epoch message decrypts");
    assert_eq!(cursor, 2);
    assert_eq!(
        alice2.current_epoch(&channel_id).unwrap(),
        epoch_before + 1,
        "the device converged on its own landed commit"
    );
    assert!(
        !alice2.has_pending_commit(&channel_id),
        "pending merged away"
    );
    assert!(!alice2.has_resumed_pending(&channel_id));
}

/// Gate-less crash reconcile, arm 2: the staged pending was persisted but the
/// send was **never accepted** (crash before/during it) — after a complete walk
/// of the log finds no matching own commit, the *resumed* pending is provably
/// undistributed and is cleared, so the interrupted operation can be retried
/// (openmls forbids staging over an unmerged pending — without the clear the
/// channel could never author again). A pending staged by the live process is
/// never touched (origin-tracked).
#[tokio::test]
async fn gate_less_pending_that_never_landed_clears_after_full_walk() {
    let secret = [9u8; 32];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([10u8; 32])).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let nest = Arc::new(MockNest::default());

    // Run 1 — stage + persist, then die before the send ever reaches the nest.
    let channel_id = {
        let alice1 = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        let (channel_id, welcome) = alice1.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let _commit = alice1.self_update(&channel_id).expect("stage");
        alice1.save_state().expect("persist the staged pending");
        channel_id
    };
    let channel_hex = channel_id.to_string();

    // Run 2 — the relaunch. The log carries only ordinary old-epoch traffic.
    let ct = bob
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: bob.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Text("old-epoch traffic".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    let alice2 = Arc::new(MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap());
    assert!(alice2.has_resumed_pending(&channel_id));
    let epoch_before = alice2.current_epoch(&channel_id).unwrap();

    let backend = Arc::new(FaunaMlsBackend::new(
        alice2.clone(),
        nest.clone(),
        "alice",
        alice2.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(!outcome.stalled);
    assert_eq!(outcome.ingested, 1, "ordinary traffic still flows");
    assert!(
        !alice2.has_pending_commit(&channel_id),
        "the undistributed resumed pending is cleared after the full walk"
    );
    assert_eq!(
        alice2.current_epoch(&channel_id).unwrap(),
        epoch_before,
        "nothing was merged"
    );
    // The interrupted operation is retryable: a fresh stage succeeds.
    alice2
        .self_update(&channel_id)
        .expect("the channel can author again");
}

/// Bug A — the `limit: 0` clamp mismatch. Every production poll driver passes
/// `page_limit = 0` (documented "whole tail in one call"), but an
/// already-deployed nest clamps `req.limit.clamp(1, 500)` → **1** — so the old
/// short-page inference (`count < page_limit` ⇒ drained) made a one-record
/// page look like a drained log after the very first record. The poll must
/// instead keep fetching until an EMPTY page: correct against such a nest with
/// no wire change, and equally against a byte-budget-shortened page (which
/// shrinks a page the same way — `transport.md` § Max frame).
#[tokio::test]
async fn poll_drains_the_whole_tail_against_a_nest_that_clamps_limit_zero_to_one() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    nest.clamp_limit_to_short_pages(true);
    let alice_actor = alice.identity_actor_id();
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let alice_thread = ThreadId("a-1".into());
    alice_backend.bind_channel(alice_thread, channel_id);
    for body in ["one", "two", "three"] {
        alice_backend
            .send(
                &fauna_mls_thread(ThreadId("a-1".into()), vec![]),
                &ComposeState {
                    body_draft: body.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("alice send ok");
    }

    let bob_manager = ConversationsManager::new();
    let bob_actor = bob.identity_actor_id();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    let mut after_seq = 0i64;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert!(!outcome.stalled);
    assert_eq!(
        outcome.ingested, 3,
        "one poll call drains the whole tail even when the nest serves \
         one-record pages (the walk must page until an empty page, not stop \
         on the first short one)"
    );
    assert_eq!(after_seq, 3, "cursor reached the log's end");
}

/// Bug A's sharpest consequence: the arm-2 gate-less crash reconcile
/// (`devices.md` § Cross-device MLS group-state sync — "a resumed pending that
/// a **complete** unstalled walk from 0 never matched is provably
/// undistributed and is cleared") ran after a walk that was NOT complete: a
/// one-record first page ended it. With the authoring device's landed commit
/// sitting at seq 2 — past that page — the reconcile cleared a pending that
/// WAS distributed, the exact outcome the ratified premise rules out (the
/// device then sits at the old epoch and every new-epoch message is silently
/// dropped). The walk must reach the log's end before the reconcile may run,
/// so the identity-matched pending merges instead.
#[tokio::test]
async fn full_walk_reconcile_never_clears_a_landed_pending_behind_a_short_first_page() {
    let secret = [21u8; 32];
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let bob = MlsEngine::new_in_memory(ActorKeypair::from_secret([22u8; 32])).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.clamp_limit_to_short_pages(true);

    // Run 1 — the crashing process: create the group, let Bob put an ordinary
    // old-epoch message at seq 1, then stage a self-update, persist, get the
    // send accepted at seq 2, and die before the merge.
    let (channel_id, channel_hex) = {
        let alice1 = MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap();
        let (channel_id, welcome) = alice1.create_group(&bob_kps).unwrap();
        bob.join_from_welcome(welcome).unwrap();
        let ct = bob
            .encrypt(
                &channel_id,
                &ChannelMessage {
                    sender: bob.identity_actor_id(),
                    sequence: 1,
                    channel_epoch: 0,
                    body: ChannelMessageBody::Text("old-epoch traffic".into()),
                    timestamp: Timestamp::now(),
                },
            )
            .unwrap();
        nest.channel_send(
            channel_id.to_string(),
            ChannelEnvelope::Application(ct).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .unwrap();
        let commit = alice1.self_update(&channel_id).expect("stage");
        alice1.save_state().expect("persist the staged pending");
        let envelope = ChannelEnvelope::Commit(commit.clone()).to_bytes().unwrap();
        nest.channel_send(channel_id.to_string(), envelope, None, vec![])
            .await
            .expect("the nest accepted the send");
        bob.process_commit(&channel_id, &commit).unwrap();
        (channel_id, channel_id.to_string())
        // alice1 drops here without merging — the crash.
    };
    // Bob speaks at the new epoch (seq 3).
    let ct = bob
        .encrypt(
            &channel_id,
            &ChannelMessage {
                sender: bob.identity_actor_id(),
                sequence: 2,
                channel_epoch: 1,
                body: ChannelMessageBody::Text("new-epoch traffic".into()),
                timestamp: Timestamp::now(),
            },
        )
        .unwrap();
    nest.channel_send(
        channel_hex.clone(),
        ChannelEnvelope::Application(ct).to_bytes().unwrap(),
        None,
        vec![],
    )
    .await
    .unwrap();

    // Run 2 — the relaunch: the pending reloads as *resumed*; the from-0 walk
    // meets [app@1, own-commit@2, app@3] behind one-record pages.
    let alice2 = Arc::new(MlsEngine::new(ActorKeypair::from_secret(secret), tmp.path()).unwrap());
    assert!(alice2.has_resumed_pending(&channel_id));
    let epoch_before = alice2.current_epoch(&channel_id).unwrap();

    let backend = Arc::new(FaunaMlsBackend::new(
        alice2.clone(),
        nest.clone(),
        "alice",
        alice2.identity_actor_id(),
    ));
    let manager = ConversationsManager::new();
    manager.register_backend(backend.clone());
    let thread_id = manager.restore_channel_slice(&empty_channel_slice(
        &channel_hex,
        "bob",
        fauna_addr("bob", bob.identity_actor_id()),
    ));
    backend.bind_channel(thread_id.clone(), channel_id);

    let mut cursor = 0i64;
    let outcome = poll_inbound_conv(&backend, &manager, &channel_id, &mut cursor, 0)
        .await
        .expect("poll");

    assert!(!outcome.stalled);
    assert_eq!(
        outcome.ingested, 2,
        "both app messages land (the walk reached past the first page)"
    );
    assert_eq!(cursor, 3, "the walk reached the log's end");
    assert_eq!(
        alice2.current_epoch(&channel_id).unwrap(),
        epoch_before + 1,
        "the landed own commit at seq 2 identity-matched and MERGED — the \
         reconcile must never clear a distributed pending on the strength of \
         an incomplete walk"
    );
    assert!(
        !alice2.has_pending_commit(&channel_id),
        "pending merged away, not cleared-and-lost"
    );
}

/// `leave_folder` (the recipient's `folder-leave-button` primitive) forgets the
/// MLS group and unmarks the folder channel, so the set drops from the
/// `has_group`-filtered member-visible list. Idempotent.
#[tokio::test]
async fn leave_folder_forgets_group_and_unmarks_channel() {
    let (nest, bob, bob_actor, _alice_hex, welcome) = folder_welcome_setup().await;
    let bob_backend = FaunaMlsBackend::new(bob.clone(), nest.clone(), "bob", bob_actor);

    let channel = join_folder_welcome(
        &bob_backend,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
        &FolderWelcomeContext::default(),
    )
    .await
    .expect("join");
    assert!(bob_backend.is_folder_channel(&channel));
    assert!(bob.has_group(&channel));

    // The member holds the raw MLS group id (from their B3 `FolderSummary` in
    // production) — leave is addressed by it, not the derived channel id.
    let raw_gid = bob
        .list_groups_with_raw_ids()
        .into_iter()
        .find(|(cid, _)| *cid == channel)
        .map(|(_, gid)| gid)
        .expect("joined group id");

    let left = leave_folder(&bob_backend, &hex::encode(&raw_gid)).expect("leave");
    assert_eq!(left, channel, "leave returns the forgotten channel");
    assert!(
        !bob_backend.is_folder_channel(&channel),
        "the folder channel is unmarked"
    );
    assert!(
        !bob.has_group(&channel),
        "the MLS group is forgotten (drops from the has_group list filter)"
    );

    // Idempotent: leaving again is a no-op success.
    let again = leave_folder(&bob_backend, &hex::encode(&raw_gid)).expect("re-leave no-op");
    assert_eq!(again, channel);
}

#[tokio::test]
async fn folder_gate_auto_joins_off_the_chat_rail() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, None);
    let gate = Arc::new(MockFolderGate::new(ArrivalDisposition::Auto));
    session.register_folder_gate(gate.clone());
    let channel = ChannelId::from_hex(&welcome.channel_hex).unwrap();

    let res = session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Folder {
                group_id_hex: String::new(),
            },
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext {
                shared_by: Some(alice_hex.clone()),
                ..Default::default()
            },
        )
        .await;

    assert!(
        res.is_ok(),
        "Auto acks (Ok) so the drain clears the welcome"
    );
    assert!(
        session.engine().has_group(&channel),
        "the MLS group is joined"
    );
    assert!(
        session.manager().snapshot().threads.is_empty(),
        "a shared folder is not a conversation — no phantom chat thread"
    );
    assert_eq!(
        gate.seen_shared_by(),
        Some(Some(alice_hex)),
        "the gate saw the nest-stamped sharer id"
    );
}

#[tokio::test]
async fn folder_gate_suppress_acks_without_joining() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, None);
    let gate = Arc::new(MockFolderGate::new(ArrivalDisposition::Suppress));
    session.register_folder_gate(gate.clone());
    let channel = ChannelId::from_hex(&welcome.channel_hex).unwrap();

    let res = session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Folder {
                group_id_hex: "aabbcc".into(),
            },
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext {
                shared_by: Some(alice_hex),
                ..Default::default()
            },
        )
        .await;

    assert!(
        res.is_ok(),
        "Suppress acks-and-drops (Ok) — the welcome is cleared"
    );
    assert!(
        !session.engine().has_group(&channel),
        "a Blocked sharer's group is never joined"
    );
    assert!(session.manager().snapshot().threads.is_empty());
    // The share rostered Bob at Welcome delivery, before the block could gate
    // anything — so suppression must ALSO drop that row, or the owner's "Shared
    // with" list keeps over-reporting and a post-unblock re-share degenerates into
    // the add path's no-op access-refresh arm (`folders.md` § Sharing → *Adding
    // the 2nd..Nth member*). Same-nest here ⇒ no relay URL.
    assert_eq!(
        gate.dropped_rosters(),
        vec![("aabbcc".to_string(), None)],
        "a suppressed knock drops the recipient's roster row, addressed by the \
         Welcome's group id"
    );
}

#[tokio::test]
async fn folder_gate_suppress_relays_the_roster_drop_cross_nest() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, None);
    let gate = Arc::new(MockFolderGate::new(ArrivalDisposition::Suppress));
    session.register_folder_gate(gate.clone());

    let res = session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Folder {
                group_id_hex: "ddeeff".into(),
            },
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            "https://home.example".into(),
            FolderWelcomeContext {
                shared_by: Some(alice_hex),
                ..Default::default()
            },
        )
        .await;

    assert!(res.is_ok());
    // A cross-nest share's roster row lives in the SET's home nest
    // (`channel_foreign_members`), so the drop must carry that URL for the
    // `fauna.federation.channel.leave` relay — the same threading a voluntary
    // cross-nest leave does.
    assert_eq!(
        gate.dropped_rosters(),
        vec![(
            "ddeeff".to_string(),
            Some("https://home.example".to_string())
        )],
        "a cross-nest suppressed knock relays the roster drop to the set's home nest"
    );
}

#[tokio::test]
async fn folder_gate_knock_retains_the_welcome_unacked() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, None);
    session.register_folder_gate(Arc::new(MockFolderGate::new(ArrivalDisposition::Knock)));
    let channel = ChannelId::from_hex(&welcome.channel_hex).unwrap();

    let res = session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Folder {
                group_id_hex: String::new(),
            },
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext {
                shared_by: Some(alice_hex),
                ..Default::default()
            },
        )
        .await;

    // `Err` is the *retain* signal (a pending-share), not a fault: the drain leaves
    // the Welcome un-acked so it stays as the `folder-pending-share`.
    assert!(
        res.is_err(),
        "Knock returns Err so the drain leaves the welcome un-acked"
    );
    assert!(
        !session.engine().has_group(&channel),
        "a stranger's group is never joined unbidden"
    );
    assert!(session.manager().snapshot().threads.is_empty());
}

#[tokio::test]
async fn folder_welcome_without_a_gate_is_retained() {
    let (nest, bob, bob_actor, alice_hex, welcome) = folder_welcome_setup().await;
    // No `register_folder_gate` — the pre-gate behaviour: a folder welcome is
    // retained un-acked (never dropped, never joined) until a gate is wired.
    let session =
        ConversationsSession::from_parts(bob, nest.clone(), "bob".into(), bob_actor, None);
    let channel = ChannelId::from_hex(&welcome.channel_hex).unwrap();

    let res = session
        .ingest_welcome_by_kind(
            WelcomeChannelKind::Folder {
                group_id_hex: String::new(),
            },
            welcome.channel_hex.clone(),
            welcome.welcome_bytes.clone(),
            String::new(),
            FolderWelcomeContext {
                shared_by: Some(alice_hex),
                ..Default::default()
            },
        )
        .await;

    assert!(res.is_err(), "no gate → Err (retained un-acked)");
    assert!(
        !session.engine().has_group(&channel),
        "not joined without a gate"
    );
}

// ── W8.4 (account-data-plane.md § Workstreams): the custody-ceremony carriage — send door, sink hand-off, no bubble ─

/// The send door seals a `ChannelMessageBody::Custody` application message the
/// peer decrypts byte-for-byte — the `send_reaction` shape, addressed by
/// channel hex (what the durable ceremony state records).
#[tokio::test]
async fn send_custody_payload_posts_envelope_peer_decrypts() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);

    let payload = vec![0x1Du8; 96];
    backend
        .send_custody_payload(&channel_id.to_string(), payload.clone())
        .await
        .expect("send_custody_payload ok");

    let envelopes = nest.sent_envelopes(&channel_id.to_string());
    assert_eq!(envelopes.len(), 1, "exactly one channel.send, no Commit");
    let mls_bytes = match ChannelEnvelope::from_bytes(&envelopes[0]).unwrap() {
        ChannelEnvelope::Application(b) => b,
        _ => panic!("expected Application envelope"),
    };
    let decrypted = bob.decrypt(&channel_id, &mls_bytes).expect("peer decrypts");
    assert!(
        matches!(decrypted.body, ChannelMessageBody::Custody(ref b) if *b == payload),
        "the ceremony bytes must survive the wire byte-for-byte, got {:?}",
        decrypted.body
    );
    assert_eq!(decrypted.sender, alice_actor);
}

/// A recording `CustodyCeremonySink` for the receive-side tests.
#[derive(Default)]
struct RecordingCustodySink {
    /// `(channel_hex, sender, bytes)` per hand-off.
    seen: std::sync::Mutex<Vec<(String, ActorId, Vec<u8>)>>,
    /// What `custody_payload` answers (durably-captured?).
    capture: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl fauna_conversations::backend::CustodyCeremonySink for RecordingCustodySink {
    async fn custody_payload(&self, channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        self.seen
            .lock()
            .unwrap()
            .push((channel_hex.to_string(), sender, bytes.to_vec()));
        self.capture.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The receive side: a custody payload reaches the registered sink as a thread
/// effect — with the MLS-authenticated sender — and never renders a bubble;
/// an unregistered sink tallies `no_sink` and equally renders nothing. The
/// tally splits the silent arms (convention 6).
#[tokio::test]
async fn a_custody_payload_reaches_the_sink_and_never_a_bubble() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    // Bob's receive side, thread bound to the channel (the Track-B shape).
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup-custody".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    let payload = vec![0x42u8; 64];
    let post = |bytes: Vec<u8>| {
        let alice = alice.clone();
        let nest = nest.clone();
        async move {
            let cm = ChannelMessage {
                sender: alice.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::Custody(bytes),
                timestamp: Timestamp::now(),
            };
            let ct = alice.encrypt(&channel_id, &cm).unwrap();
            nest.channel_send(
                channel_id.to_string(),
                ChannelEnvelope::Application(ct).to_bytes().unwrap(),
                None,
                vec![],
            )
            .await
            .unwrap();
        }
    };

    // (1) No sink registered: skipped, tallied, no bubble.
    post(payload.clone()).await;
    let mut after_seq = 0i64;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(outcome.ingested, 0, "a ceremony payload is never a bubble");
    let counts = bob_backend.custody_payload_counts();
    assert_eq!((counts.seen, counts.no_sink), (1, 1), "{counts:?}");

    // (2) Sink registered and capturing: the payload arrives with the
    // MLS-authenticated sender and the carrying channel's hex.
    let sink = Arc::new(RecordingCustodySink::default());
    sink.capture
        .store(true, std::sync::atomic::Ordering::Relaxed);
    bob_backend.set_custody_ceremony_sink(sink.clone());
    post(payload.clone()).await;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(outcome.ingested, 0, "still never a bubble");
    {
        let seen = sink.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let (hex, sender, bytes) = &seen[0];
        assert_eq!(hex, &channel_id.to_string());
        assert_eq!(sender, &alice_actor, "the MLS-authenticated sender");
        assert_eq!(bytes, &payload, "verbatim bytes");
    }
    let counts = bob_backend.custody_payload_counts();
    assert_eq!((counts.seen, counts.captured), (2, 1), "{counts:?}");

    // (3) A sink that cannot capture: tallied uncaptured, walk unstalled.
    sink.capture
        .store(false, std::sync::atomic::Ordering::Relaxed);
    post(payload.clone()).await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok — a capture failure never stalls the walk");
    let counts = bob_backend.custody_payload_counts();
    assert_eq!(
        (counts.seen, counts.captured, counts.uncaptured),
        (3, 1, 1),
        "{counts:?}"
    );

    // And through it all: no custody bubble ever rendered.
    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    assert!(
        detail.messages.iter().all(|m| m.body == "<<setup>>"),
        "no ceremony payload may surface in the transcript"
    );
}

/// A `ShareEndpointsSink` that does what a real one does: bind the received
/// advertisement to the MLS-authenticated sender and the carrying channel
/// (`fauna_peer_share::bind_share_advertisement`), and record the row it
/// would have written — or the refusal.
#[derive(Default)]
struct BindingShareEndpointsSink {
    /// The `(entry_key, row)` pairs a real sink would have persisted.
    bound: std::sync::Mutex<Vec<(String, fauna_core::share_endpoints::ShareEndpoints)>>,
    /// Every refusal, for the assertion that lies are counted as lies.
    refused: std::sync::Mutex<Vec<fauna_peer_share::AdvertisementRefusal>>,
}

#[async_trait]
impl fauna_conversations::backend::ShareEndpointsSink for BindingShareEndpointsSink {
    async fn share_endpoints(&self, channel_hex: &str, sender: ActorId, bytes: &[u8]) -> bool {
        let Ok(advertised) = fauna_core::encoding::canonical_decode::<
            fauna_core::share_endpoints::ShareEndpoints,
        >(bytes) else {
            return false;
        };
        let Ok(channel) = fauna_mls::types::ChannelId::from_hex(channel_hex) else {
            return false;
        };
        match fauna_peer_share::bind_share_advertisement(&advertised, sender, &channel.0) {
            Ok((key, row)) => {
                self.bound.lock().unwrap().push((key, row));
                true
            }
            Err(refusal) => {
                self.refused.lock().unwrap().push(refusal);
                false
            }
        }
    }
}

/// The discovery carriage, end to end over a real MLS group (slice F).
///
/// Three facts in one walk, because they only mean anything together:
///
/// 1. An honest member's advertisement reaches the sink with the
///    **MLS-authenticated** sender and binds to a dial row keyed by that
///    proven identity — the row `share_dial_targets` later reads.
/// 2. A member lying about **who** it is — advertising a third member's
///    location over its own admitted channel — is **refused**, not repaired.
///    This is the property the whole binding exists for: without it any
///    member of a set could durably redirect every other member's dials at a
///    box it controls, and the lie would be indistinguishable from a fact
///    because the carrying channel really is authenticated.
/// 3. Neither ever renders a chat bubble, and a refusal never stalls the
///    walk — the `Custody` discipline, which this variant inherits verbatim.
#[tokio::test]
async fn a_share_endpoint_advertisement_binds_to_its_authenticated_sender_and_never_a_bubble() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel_id, welcome) = alice.create_group(&bob_kps).unwrap();
    bob.join_from_welcome(welcome).unwrap();

    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();

    // Bob's receive side, thread bound to the channel (the Track-B shape).
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(
        bob.clone(),
        nest.clone(),
        "bob",
        bob_actor,
    ));
    bob_manager.register_backend(bob_backend.clone());
    bob_manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr("alice", alice_actor),
            recipients: vec![fauna_addr("bob", bob_actor)],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId("setup-share-endpoints".into()),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let bob_thread_id = bob_manager.snapshot().threads[0].thread_id.clone();
    bob_backend.bind_channel(bob_thread_id.clone(), channel_id);

    let candidates = fauna_core::device_endpoints::DeviceEndpoints {
        node_id: alice_actor.0,
        lan_addrs: vec!["192.168.1.9:4433".into()],
        public_addrs: vec!["203.0.113.9:4433".into()],
        relay_url: Some("https://relay.example/".into()),
    };

    let post = |body_bytes: Vec<u8>| {
        let alice = alice.clone();
        let nest = nest.clone();
        async move {
            let cm = ChannelMessage {
                sender: alice.identity_actor_id(),
                sequence: 1,
                channel_epoch: 0,
                body: ChannelMessageBody::ShareEndpoints(body_bytes),
                timestamp: Timestamp::now(),
            };
            let ct = alice.encrypt(&channel_id, &cm).unwrap();
            nest.channel_send(
                channel_id.to_string(),
                ChannelEnvelope::Application(ct).to_bytes().unwrap(),
                None,
                vec![],
            )
            .await
            .unwrap();
        }
    };

    // (1) No sink registered: skipped, tallied, no bubble — and the
    //     nest-mediated path is unaffected, which is why nothing is re-driven.
    let honest =
        fauna_peer_share::own_advertisement(&channel_id.0, &alice_actor, candidates.clone());
    let honest_bytes = fauna_core::encoding::canonical_encode(&honest).unwrap();
    post(honest_bytes.clone()).await;
    let mut after_seq = 0i64;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(outcome.ingested, 0, "an advertisement is never a bubble");
    let counts = bob_backend.share_endpoints_counts();
    assert_eq!((counts.seen, counts.no_sink), (1, 1), "{counts:?}");

    // (2) Sink registered: the honest advertisement binds to a dial row keyed
    //     by the MLS-authenticated sender.
    let sink = Arc::new(BindingShareEndpointsSink::default());
    bob_backend.set_share_endpoints_sink(sink.clone());
    post(honest_bytes).await;
    let outcome = poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(outcome.ingested, 0, "still never a bubble");
    {
        let bound = sink.bound.lock().unwrap();
        assert_eq!(bound.len(), 1, "the honest advertisement binds");
        let (key, row) = &bound[0];
        assert_eq!(
            key,
            &fauna_core::share_endpoints::share_entry_key(&channel_id.0, &alice_actor.0),
            "keyed by the set and the PROVEN advertiser"
        );
        assert_eq!(row.member_actor, alice_actor.0.to_vec());
        assert_eq!(row.endpoints.node_id, alice_actor.0);
    }
    let counts = bob_backend.share_endpoints_counts();
    assert_eq!((counts.seen, counts.captured), (2, 1), "{counts:?}");

    // (3) Alice lies: a patched client advertises CAROL's location over the
    //     set Alice really is admitted to. The channel authenticates Alice,
    //     so nothing below the binding can catch this — and the binding does.
    let carol = ActorId([0xC0; 32]);
    let lie = fauna_core::share_endpoints::ShareEndpoints {
        channel_id: channel_id.0.to_vec(),
        member_actor: carol.0.to_vec(),
        endpoints: fauna_core::device_endpoints::DeviceEndpoints {
            node_id: carol.0,
            lan_addrs: vec!["192.168.1.66:4433".into()],
            public_addrs: vec![],
            relay_url: None,
        },
    };
    post(fauna_core::encoding::canonical_encode(&lie).unwrap()).await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok — a refused advertisement never stalls the walk");
    {
        let refused = sink.refused.lock().unwrap();
        assert_eq!(
            refused.as_slice(),
            &[fauna_peer_share::AdvertisementRefusal::NotTheSender],
            "a member advertising a THIRD member's location must be refused"
        );
        assert_eq!(
            sink.bound.lock().unwrap().len(),
            1,
            "and must not have written a row under Carol's name"
        );
    }
    let counts = bob_backend.share_endpoints_counts();
    assert_eq!(
        (counts.seen, counts.captured, counts.uncaptured),
        (3, 1, 1),
        "a refusal is tallied as uncaptured — a climbing number here is a \
         security signal, not a storage one: {counts:?}"
    );

    // (4) The FOLDER rail routes advertisements too. A shared set's channel
    //     is polled in production by `poll_inbound_folder`, not
    //     `poll_inbound_conv` — and the folder rail shipped skipping every
    //     Application envelope, so the app-level two-actor journey found the
    //     member never caching a dial row (2026-08-19). The 0-seeded cursor
    //     re-walks the three records above; their ratchet secrets are
    //     consumed (a decrypted MLS application message cannot be decrypted
    //     twice), so they skip — exactly a fresh launch's re-walk — and only
    //     the new post routes.
    let honest_again = fauna_peer_share::own_advertisement(&channel_id.0, &alice_actor, candidates);
    post(fauna_core::encoding::canonical_encode(&honest_again).unwrap()).await;
    let mut folder_cursor = 0i64;
    let outcome = poll_inbound_folder(&bob_backend, &channel_id, &mut folder_cursor, 0)
        .await
        .expect("folder poll ok");
    assert!(
        !outcome.stalled,
        "an advertisement never stalls the commit walk"
    );
    assert_eq!(
        sink.bound.lock().unwrap().len(),
        2,
        "the folder rail must bind the advertisement exactly as the conv rail does"
    );
    let counts = bob_backend.share_endpoints_counts();
    assert_eq!(
        (counts.seen, counts.captured),
        (4, 2),
        "one new advertisement seen and captured through the folder rail: {counts:?}"
    );

    // And through it all: no advertisement ever surfaced in the transcript.
    let detail = bob_manager.thread_detail(bob_thread_id).expect("thread");
    assert!(
        detail.messages.iter().all(|m| m.body == "<<setup>>"),
        "no endpoint advertisement may surface in the transcript"
    );
}

// ── The room model: governed rooms (conversation-rooms.md § Roles and ─────
// authorization, § Join rules and invites, § History for joiners) ───────────
//
// A **governed** room carries an owner-signed policy in its MLS group context
// (`fauna_mls::room_policy`); a **policy-less** room carries none and keeps the open
// membership of an ungoverned group. These tests drive the shared-Rust
// consumption of that policy — the client-side refusals an honest app makes
// BEFORE authoring a commit, the projection every app paints, the roster
// report the committing device owes, and the history a newcomer receives —
// through the same `MockNest` seam as the tracks above.

use fauna_conversations::backend::{
    RoomPolicyEdit, RoomRosterKnownMember, RoomRosterReader, RoomRosterReport,
    RoomRosterReportOutcome, RoomRosterReporter,
};
use fauna_conversations::eviction::UnreachableSeatClass;
use fauna_conversations::room::{HistoryPolicy, JoinRule, RoomClass, RoomRole};
use fauna_i18n::strings::error::send as send_errors;
use fauna_mls::room_policy::{RecordedSuccession, RoomPolicy, RoomPolicyExtension};
use fauna_mls::succession::{commit_add_successor, commit_remove_old};

/// One member's seat: a backend + manager over the shared nest, the thread
/// materialized channel-keyed and bound.
struct Seat {
    backend: Arc<FaunaMlsBackend>,
    manager: Arc<ConversationsManager>,
    tid: ThreadId,
    actor: ActorId,
}

fn seat(
    engine: Arc<MlsEngine>,
    nest: &Arc<MockNest>,
    name: &str,
    channel_id: ChannelId,
    others: Vec<TypedAddress>,
) -> Seat {
    let actor = engine.identity_actor_id();
    let manager = ConversationsManager::new();
    let tid = manager.materialize_conv_thread(channel_id.to_string(), others);
    let backend = Arc::new(FaunaMlsBackend::new(
        engine.clone(),
        nest.clone(),
        name,
        actor,
    ));
    backend.bind_channel(tid.clone(), channel_id);
    manager.register_backend(backend.clone());
    Seat {
        backend,
        manager,
        tid,
        actor,
    }
}

/// Alice's governed room with bob and carol: every engine joined, the channel
/// id, and the shared nest.
struct Governed {
    alice: Arc<MlsEngine>,
    bob: Arc<MlsEngine>,
    carol: Arc<MlsEngine>,
    channel_id: ChannelId,
    nest: Arc<MockNest>,
}

fn governed_room() -> Governed {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let carol = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let policy = RoomPolicy::initial(alice.identity_actor_id(), None);
    let signed = alice.sign_room_policy(&policy).unwrap();
    let mut kps = bob.generate_key_packages(1).unwrap();
    kps.extend(carol.generate_key_packages(1).unwrap());
    let (channel_id, welcome) = alice
        .create_group_with_policy(&kps, &RoomPolicyExtension::new(signed))
        .unwrap();
    bob.join_from_welcome(welcome.clone()).unwrap();
    carol.join_from_welcome(welcome).unwrap();
    Governed {
        alice,
        bob,
        carol,
        channel_id,
        nest: Arc::new(MockNest::default()),
    }
}

fn alice_seat(g: &Governed) -> Seat {
    seat(
        g.alice.clone(),
        &g.nest,
        "alice",
        g.channel_id,
        vec![
            fauna_addr("bob", g.bob.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    )
}

fn carol_seat(g: &Governed) -> Seat {
    seat(
        g.carol.clone(),
        &g.nest,
        "carol",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("bob", g.bob.identity_actor_id()),
        ],
    )
}

/// Every commit the nest holds for `channel`, oldest first, processed on
/// `engine` — a member catching up on the log the way the poll does.
fn catch_up(engine: &MlsEngine, nest: &MockNest, channel: &ChannelId) {
    for env in nest.sent_envelopes(&channel.to_string()) {
        if let Ok(ChannelEnvelope::Commit(cb)) = ChannelEnvelope::from_bytes(&env) {
            match engine.process_commit(channel, &cb) {
                Ok(()) | Err(fauna_mls::error::MlsError::PastEpochCommit) => {}
                Err(e) => panic!("catch-up commit failed: {e:?}"),
            }
        }
    }
}

fn page_error_message(manager: &ConversationsManager) -> (String, String) {
    let error = manager
        .snapshot()
        .error
        .expect("the refusal must surface on the page's error-message");
    let message = error.args.get("message").cloned().unwrap_or_default();
    (error.key, message)
}

/// Post `body` on `channel` as `engine`, sealed by the raw engine — a member
/// whose app is not under test.
async fn post_raw(g: &Governed, engine: &MlsEngine, body: ChannelMessageBody) {
    let message = ChannelMessage {
        sender: engine.identity_actor_id(),
        sequence: 1,
        channel_epoch: 0,
        body,
        timestamp: Timestamp::now(),
    };
    let ct = engine.encrypt(&g.channel_id, &message).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Application(ct).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .unwrap();
}

/// `conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism*, the end-to-end class, through the production seam on both
/// sides (snapshot flag → `delete_message` → the sealed `Delete` → a peer's
/// decrypt → `poll_inbound_conv` → projection): the owner's delete of a
/// member's message tombstones it on another member's seat, and a plain
/// member's delete of another member's message is still the forged delete it
/// always was. The role is the one the engine read at decrypt, from the group
/// context of the epoch the delete was sealed in — with that read removed
/// (`DeleteClaim::admits` back to the sender match alone) the first half fails.
#[tokio::test]
async fn an_owners_delete_of_a_members_message_tombstones_it_and_a_members_does_not() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let carol = carol_seat(&g);
    // Bob — a plain member — posts twice: seq 1 and seq 2.
    post_raw(&g, &g.bob, ChannelMessageBody::Text("bob one".into())).await;
    post_raw(&g, &g.bob, ChannelMessageBody::Text("bob two".into())).await;
    let hex = g.channel_id.to_string();
    let one = MessageId(format!("conv:{hex}:1"));
    let two = MessageId(format!("conv:{hex}:2"));

    let (mut alice_seq, mut carol_seq) = (0i64, 0i64);
    poll_inbound_conv(
        &alice.backend,
        &alice.manager,
        &g.channel_id,
        &mut alice_seq,
        0,
    )
    .await
    .expect("alice polls");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut carol_seq,
        0,
    )
    .await
    .expect("carol polls");
    let bubble = |seat: &Seat, id: &MessageId| {
        seat.manager
            .thread_detail(seat.tid.clone())
            .expect("thread")
            .messages
            .into_iter()
            .find(|m| &m.message_id == id)
            .expect("the bubble is held")
    };
    assert!(
        bubble(&alice, &one).can_delete,
        "the owner is offered delete on a member's message"
    );
    assert!(
        !bubble(&carol, &one).can_delete,
        "a plain member is not offered delete on another member's message"
    );

    // The owner deletes bob's first message through the manager.
    alice
        .manager
        .delete_message(alice.tid.clone(), one.clone())
        .await;
    // Carol — a plain member — forges a delete of bob's second.
    post_raw(&g, &g.carol, ChannelMessageBody::Delete { target_seq: 2 }).await;
    // A refused gesture posts nothing: carol's own manager drops it at the door.
    let before = g.nest.sent_envelopes(&hex).len();
    carol
        .manager
        .delete_message(carol.tid.clone(), two.clone())
        .await;
    assert_eq!(
        g.nest.sent_envelopes(&hex).len(),
        before,
        "a member's manager refuses a cross-sender delete before any send"
    );

    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut carol_seq,
        0,
    )
    .await
    .expect("carol polls the owner's delete");
    poll_inbound_conv(
        &alice.backend,
        &alice.manager,
        &g.channel_id,
        &mut alice_seq,
        0,
    )
    .await
    .expect("alice polls the member's forged delete");
    assert!(
        bubble(&carol, &one).deleted,
        "the owner's delete tombstones the member's message on another member's seat"
    );
    assert!(
        !bubble(&alice, &two).deleted,
        "a plain member's cross-sender delete is dropped as forged"
    );
    assert!(
        !bubble(&carol, &two).deleted,
        "and the forger's own seat paints no tombstone either"
    );
}

#[tokio::test]
async fn bootstrap_group_mints_a_governed_room_owned_by_its_creator() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    for peer in [&bob, &carol] {
        nest.seed_keypackage(
            &hex::encode(peer.identity_actor_id().0),
            peer.generate_key_packages_bytes(1).unwrap()[0].clone(),
        );
    }
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread = fauna_mls_thread(
        ThreadId("g-1".into()),
        vec![
            fauna_addr("bob", bob.identity_actor_id()),
            fauna_addr("carol", carol.identity_actor_id()),
        ],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello room".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("bootstrap+send ok");

    let channel_hex = backend
        .channel_binding_hex(&thread.thread_id)
        .expect("the thread bound its channel");
    let channel_id = channel_id_from_hex(&channel_hex);
    let policy = alice
        .room_policy(&channel_id)
        .expect("held")
        .expect("decodes");
    assert_eq!(
        policy.signed.policy.owner, alice_actor,
        "born owned by its creator"
    );
    assert_eq!(
        policy.signed.policy.join_rule,
        fauna_mls::room_policy::JoinRule::Invite
    );

    // The projection every app paints: class, roles, the viewer's own role.
    let room = backend
        .room_state(&thread)
        .expect("a FaunaMls thread is a room");
    assert_eq!(room.class, RoomClass::EndToEnd);
    assert_eq!(room.my_role, Some(RoomRole::Owner));
    assert_eq!(
        room.members.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![Some(RoomRole::Member), Some(RoomRole::Member)]
    );
    let caps = backend.capabilities(&thread);
    assert!(caps.can_invite && caps.can_remove_members && caps.can_set_policy);
    assert!(caps.can_appoint_admins);

    // The joiners hold the same policy from their Welcome alone.
    let welcomes = nest.welcomes();
    bob.join_from_welcome_bytes(&welcomes[0].welcome_bytes)
        .unwrap();
    assert_eq!(
        bob.room_policy(&channel_id)
            .expect("held")
            .expect("decodes"),
        policy
    );
}

/// Every end-to-end group is born governed. A peer package that does not
/// advertise the room-policy extension (a non-conforming client's — every
/// current app's package does) makes the fork fail with the engine's by-name
/// refusal; it is never born policy-less instead (that older-app fallback was
/// a compat remnant, removed 2026-09-25 — `version-compatibility.md`
/// § Dimension 2, the fourth exception).
#[tokio::test]
async fn bootstrap_group_refuses_a_peer_whose_key_package_lacks_the_policy_extension() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    // Bob's package advertises no room-policy capability.
    nest.seed_keypackage(
        &hex::encode(bob.identity_actor_id().0),
        bob.generate_policy_blind_key_package_bytes_for_test()
            .unwrap(),
    );
    nest.seed_keypackage(
        &hex::encode(carol.identity_actor_id().0),
        carol.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let thread = fauna_mls_thread(
        ThreadId("g-1".into()),
        vec![
            fauna_addr("bob", bob.identity_actor_id()),
            fauna_addr("carol", carol.identity_actor_id()),
        ],
    );
    let err = backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect_err("no policy-less fallback: the fork is refused");
    assert!(
        err.to_string()
            .contains("does not advertise the room-policy extension"),
        "the engine's by-name refusal surfaces: {err}"
    );
    assert!(
        backend.channel_binding_hex(&thread.thread_id).is_none(),
        "no channel is bound to the refused fork"
    );
    assert!(nest.welcomes().is_empty(), "no Welcome is delivered");
}

/// A [`RoomRosterReporter`] that also records, per report, how many envelopes
/// the mock nest had seen on the room's channel at the moment the report was
/// made — the witness that the birth report follows the first post, which is
/// what registers the creator on the channel's routing roster at the room's
/// home nest (the report door's first gate).
struct OrderingReporter {
    nest: Arc<MockNest>,
    reports: Mutex<Vec<(RoomRosterReport, usize)>>,
}

#[async_trait]
impl RoomRosterReporter for OrderingReporter {
    async fn report(&self, report: RoomRosterReport) -> RoomRosterReportOutcome {
        let seen = self.nest.sent_envelopes(&report.channel_hex).len();
        self.reports.lock().unwrap().push((report, seen));
        RoomRosterReportOutcome::Stored
    }
}

/// Every end-to-end room reports the roster its creation produced — the
/// **birth report** (`conversation-rooms.md` § The floor roster → *End-to-end
/// rooms*), riding the creating device's first send. A 1:1 is the case this
/// exists for: it carries no policy and never a later membership commit (its
/// add-participant forks a new group), so the birth report is the only one it
/// will ever make — before it, a 1:1's floor roster was never written at all,
/// and the custody serve door failed closed for every DM.
#[tokio::test]
async fn a_fresh_1_1_reports_its_role_less_birth_roster_once_after_its_first_post() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob.identity_actor_id().0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let reporter = Arc::new(OrderingReporter {
        nest: nest.clone(),
        reports: Mutex::new(Vec::new()),
    });
    backend.set_room_roster_reporter(reporter.clone());
    let thread = fauna_mls_thread(
        ThreadId("dm-1".into()),
        vec![fauna_addr("bob", bob.identity_actor_id())],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("the first send bootstraps the 1:1");
    let channel_id = channel_id_from_hex(&backend.channel_binding_hex(&thread.thread_id).unwrap());
    assert!(
        alice.room_policy(&channel_id).is_none(),
        "a 1:1 carries no policy"
    );

    let counts = backend.roster_report_counts();
    assert_eq!(
        (counts.owed, counts.delivered, counts.no_reporter),
        (1, 1, 0),
        "the birth report is owed and delivered"
    );
    {
        let reports = reporter.reports.lock().unwrap();
        assert_eq!(reports.len(), 1, "one birth report");
        let (report, envelopes_seen) = &reports[0];
        assert_eq!(
            *envelopes_seen, 1,
            "the birth report follows the first post — the send is what puts the \
             creator on the channel's routing roster, which the report door's \
             first gate requires"
        );
        assert_eq!(report.channel_hex, channel_id.to_string());
        assert_eq!(report.policy_version, None, "no policy, no version");
        assert_eq!(
            report.commit_seq, None,
            "the group's creation is not on the room log: the birth report is the \
             one report with no position to name, and the home nest's bootstrap \
             bound admits it on an empty floor"
        );
        assert_eq!(
            report.home_nest_url, None,
            "the creator's channel is same-nest by construction"
        );
        let mut members: Vec<(ActorId, Option<RoomRole>)> =
            report.members.iter().map(|m| (m.actor, m.role)).collect();
        members.sort_by_key(|(a, _)| a.0);
        let mut expected = vec![(alice_actor, None), (bob.identity_actor_id(), None)];
        expected.sort_by_key(|(a, _)| a.0);
        assert_eq!(members, expected, "both principals, role-less");
    }

    // A second send on the bound channel is not a birth: nothing more is owed.
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "again".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("an ordinary send");
    assert_eq!(backend.roster_report_counts().owed, 1);
    assert_eq!(reporter.reports.lock().unwrap().len(), 1);
}

/// The birth report is every end-to-end room's, not a 1:1 special case — a
/// fresh group had the identical hole (no floor roster until its first later
/// commit). A governed group reports the roles its birth policy gives, at
/// version 1 (every group is born governed; the role-less report a
/// policy-less room makes is the 1:1's, pinned above, and the door's own
/// `conformance_conversation_rooms.rs::a_policy_less_room_reports_members_with_no_roles`).
#[tokio::test]
async fn a_fresh_group_reports_its_birth_roster_with_the_roles_its_policy_gives() {
    async fn birth_report() -> (ActorId, ActorId, ActorId, RoomRosterReport) {
        let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
        let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let carol = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
        let nest = Arc::new(MockNest::default());
        nest.seed_keypackage(
            &hex::encode(bob.identity_actor_id().0),
            bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
        );
        nest.seed_keypackage(
            &hex::encode(carol.identity_actor_id().0),
            carol.generate_key_packages_bytes(1).unwrap()[0].clone(),
        );
        let alice_actor = alice.identity_actor_id();
        let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
        let reporter = Arc::new(RecordingReporter::default());
        backend.set_room_roster_reporter(reporter.clone());
        let thread = fauna_mls_thread(
            ThreadId("g-1".into()),
            vec![
                fauna_addr("bob", bob.identity_actor_id()),
                fauna_addr("carol", carol.identity_actor_id()),
            ],
        );
        backend
            .send(
                &thread,
                &ComposeState {
                    body_draft: "hello".into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("the first send bootstraps the group");
        let reports = reporter.reports.lock().unwrap();
        assert_eq!(reports.len(), 1, "one birth report");
        (
            alice_actor,
            bob.identity_actor_id(),
            carol.identity_actor_id(),
            reports[0].clone(),
        )
    }

    let (alice, bob, carol, governed) = birth_report().await;
    assert_eq!(
        governed.policy_version,
        Some(1),
        "born governed at version 1"
    );
    assert_eq!(governed.commit_seq, None);
    let mut roles: Vec<(ActorId, Option<RoomRole>)> =
        governed.members.iter().map(|m| (m.actor, m.role)).collect();
    roles.sort_by_key(|(a, _)| a.0);
    let mut expected = vec![
        (alice, Some(RoomRole::Owner)),
        (bob, Some(RoomRole::Member)),
        (carol, Some(RoomRole::Member)),
    ];
    expected.sort_by_key(|(a, _)| a.0);
    assert_eq!(
        roles, expected,
        "the birth policy's roles: owner and members"
    );
}

#[tokio::test]
async fn a_member_cannot_remove_and_the_page_error_says_so() {
    let g = governed_room();
    let carol = carol_seat(&g);
    let bob_addr = fauna_addr("bob", g.bob.identity_actor_id());

    // The projection already says no…
    let detail = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert_eq!(
        detail.room.as_ref().unwrap().my_role,
        Some(RoomRole::Member)
    );
    assert!(!detail.capabilities.can_remove_members);
    assert!(!detail.capabilities.can_invite, "invite-only room");

    // …and the gesture is refused BEFORE any commit exists.
    carol
        .manager
        .remove_participant(carol.tid.clone(), bob_addr.clone())
        .await;
    let (key, message) = page_error_message(&carol.manager);
    assert_eq!(key, "conversations.unified.error_remove_participant");
    assert_eq!(message, send_errors::ROOM_REMOVE_NOT_PERMITTED);
    assert!(
        g.nest.sent_envelopes(&g.channel_id.to_string()).is_empty(),
        "no commit was authored"
    );
    let detail = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert!(
        detail
            .participants
            .iter()
            .any(|p| p.same_participant(&bob_addr)),
        "bob is put back on the roster"
    );

    // Nor may a member invite under `invite`.
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    g.nest.seed_keypackage(
        &hex::encode(dave.identity_actor_id().0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let err = carol
        .backend
        .add_participant(
            carol.tid.clone(),
            fauna_addr("dave", dave.identity_actor_id()),
        )
        .await
        .expect_err("a member's invite is refused under `invite`");
    assert_eq!(err.user_detail(), send_errors::ROOM_INVITE_NOT_PERMITTED);
    assert!(g.nest.welcomes().is_empty());
}

/// A recording [`RoomRosterReporter`].
#[derive(Default)]
struct RecordingReporter {
    reports: Mutex<Vec<RoomRosterReport>>,
}

#[async_trait]
impl RoomRosterReporter for RecordingReporter {
    async fn report(&self, report: RoomRosterReport) -> RoomRosterReportOutcome {
        self.reports.lock().unwrap().push(report);
        RoomRosterReportOutcome::Stored
    }
}

/// One scripted answer to a floor-roster read — richer than
/// [`ScriptedReader::new`]'s `Option` shorthand, which has no way to script a
/// read that genuinely FAILED (`RoomRosterRead::Unavailable`) distinctly from
/// one that confirmed the floor absent (`RoomRosterRead::NoFloor`); see
/// [`ScriptedReader::new_scripted`].
#[derive(Clone)]
enum ScriptedRosterAnswer {
    Floor(Vec<RoomRosterKnownMember>),
    NoFloor,
    Unavailable,
}

impl From<Option<Vec<RoomRosterKnownMember>>> for ScriptedRosterAnswer {
    fn from(answer: Option<Vec<RoomRosterKnownMember>>) -> Self {
        match answer {
            Some(members) => ScriptedRosterAnswer::Floor(members),
            None => ScriptedRosterAnswer::NoFloor,
        }
    }
}

/// A scripted [`RoomRosterReader`] — one answer per call, a count of how many
/// times it was asked, and the `home_nest_url` it was asked WITH on each call
/// (the seam's same-nest-vs-relay pick, which the backend makes).
struct ScriptedReader {
    answers: Mutex<std::collections::VecDeque<ScriptedRosterAnswer>>,
    calls: std::sync::atomic::AtomicUsize,
    homes: Mutex<Vec<Option<String>>>,
    /// The policy version every answered floor carries — a room founded by
    /// `room.create` is born at version 1.
    policy_version: Option<u64>,
    /// The signed policy the read serves back, canonical dag-cbor.
    policy: Mutex<Option<Vec<u8>>>,
}

impl ScriptedReader {
    /// `None` scripts a *confirmed* empty floor (`RoomRosterRead::NoFloor`) —
    /// the vocabulary every existing caller of this constructor already means
    /// by it. [`Self::new_scripted`] can additionally script a read that
    /// simply FAILED (`RoomRosterRead::Unavailable`), which this shorthand
    /// cannot express.
    fn new(answers: Vec<Option<Vec<RoomRosterKnownMember>>>) -> Self {
        Self::new_scripted(answers.into_iter().map(Into::into).collect())
    }
    fn new_scripted(answers: Vec<ScriptedRosterAnswer>) -> Self {
        Self {
            answers: Mutex::new(answers.into()),
            calls: std::sync::atomic::AtomicUsize::new(0),
            homes: Mutex::new(Vec::new()),
            policy_version: Some(1),
            policy: Mutex::new(None),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// The `home_nest_url` of every call so far, in order.
    fn homes(&self) -> Vec<Option<String>> {
        self.homes.lock().unwrap().clone()
    }
}

#[async_trait]
impl RoomRosterReader for ScriptedReader {
    async fn read_roster(
        &self,
        _channel_hex: String,
        home_nest_url: Option<String>,
    ) -> RoomRosterRead {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.homes.lock().unwrap().push(home_nest_url);
        match self.answers.lock().unwrap().pop_front() {
            Some(ScriptedRosterAnswer::Floor(members)) => RoomRosterRead::Floor(RoomFloor {
                members,
                policy_version: self.policy_version,
                policy: self.policy.lock().unwrap().clone(),
                labelers: None,
            }),
            // A dry queue behaves as `new`'s own `None` always has: a
            // confirmed-empty floor, not a failure.
            Some(ScriptedRosterAnswer::NoFloor) | None => RoomRosterRead::NoFloor,
            Some(ScriptedRosterAnswer::Unavailable) => RoomRosterRead::Unavailable,
        }
    }
}

/// A **user** row of a floor roster carrying no wrap target — what the handle
/// read is about. `room_row` below is the keyed twin, for the mint.
fn known(actor: ActorId, handle: Option<&str>) -> RoomRosterKnownMember {
    RoomRosterKnownMember {
        actor,
        handle: handle.map(str::to_string),
        domain: handle.map(|_| "nest.test".to_string()),
        kind: RoomPrincipalKind::User,
        role: None,
        entry_id: None,
        reception_pubkey: None,
        joined_at_ms: 1_700_000_000_000,
        tip_wrapped: None,
    }
}

/// **A member named once, then removed and re-admitted, is named again.**
///
/// `RosterAnswer::Named` used to mean "done for the session — a later read could
/// only repeat it". That stopped being true the day the reconcile's add arm
/// began seating a member another device added: removal drops the row, and the
/// re-admission seats it again handle-less, while the per-actor answer cache
/// still says `Named`. The read filtered the member out, so a re-admitted member
/// stayed elided for the rest of the session — flakily, because whether the
/// first naming went through the cached roster read or the uncached seat-time
/// scan was a race (`test_conversation_room_roles.py`'s re-admit step failed
/// three runs in four on exactly this).
///
/// A `Named` answer is only consulted for an actor rendering nameless NOW, so
/// there it is stale by construction, and the read asks again.
#[tokio::test]
async fn a_member_named_then_removed_and_readmitted_is_named_again() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let stranger = ActorId([77u8; 32]);
    let roster_with = [bob.actor, g.alice.identity_actor_id(), stranger];
    let roster_without = [bob.actor, g.alice.identity_actor_id()];
    let named = || {
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(stranger, Some("frank")),
        ])
    };
    let reader = Arc::new(ScriptedReader::new(vec![named(), named()]));
    bob.backend.set_room_roster_reader(reader.clone());

    // Seated off the agreed roster, then named by the floor read.
    bob.manager
        .apply_inbound_roster(bob.tid.clone(), &roster_with, &[]);
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(reader.calls(), 1);
    assert!(
        bob.manager.nameless_participants(&bob.tid).is_empty(),
        "the first read names the member"
    );

    // Removed by another member's commit, then re-admitted: the add arm seats
    // them again with no handle, because the engine roster carries none.
    bob.manager
        .apply_inbound_roster(bob.tid.clone(), &roster_without, &[]);
    bob.manager
        .apply_inbound_roster(bob.tid.clone(), &roster_with, &[]);
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![stranger],
        "the re-admitted member arrives handle-less"
    );

    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(
        reader.calls(),
        2,
        "a member nameless again must be read again — a `Named` answer for them \
         is stale, not final"
    );
    assert!(
        bob.manager.nameless_participants(&bob.tid).is_empty(),
        "the re-admitted member is named again: {:?}",
        bob.manager.nameless_participants(&bob.tid)
    );
}

/// **The id-keyed handle read asks again about a member the roster did not
/// mention, and stops asking about one it did** — the caching rule that keeps
/// the read bounded to membership events without pinning a member as
/// permanently nameless (`conversation-rooms.md` § Implementation status
/// today, the roster bullet).
///
/// The distinction is the whole of it, and the reason it is not "record every
/// actor we asked about": the floor roster is a **member-reported mirror**, so
/// this device routinely folds a membership commit BEFORE the committing
/// device's report of it reaches the nest. A roster that predates the newest
/// member omits them — and treating that omission as the answer "no handle"
/// would pin the one member this whole path exists for as elided for the rest
/// of the session, precisely in the common case.
///
/// A member the roster DID list without a handle — one homed on another nest
/// whose own home nest has not announced it yet — is answered *for now*: its
/// name rides that member's own next drain (`federation.md` § Cross-nest
/// shared folders + channel append, the id→handle bullet), so the read asks
/// again after a gap of polls rather than at poll-tick rate (the gap is the
/// subject of the next test). A read that FAILS records nothing.
#[tokio::test]
async fn the_handle_read_reasks_after_a_roster_that_omitted_the_member() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let stranger = ActorId([77u8; 32]);
    let foreigner = ActorId([88u8; 32]);
    // Two members this device has never met arrive off the agreed roster.
    bob.manager.apply_inbound_roster(
        bob.tid.clone(),
        &[bob.actor, g.alice.identity_actor_id(), stranger, foreigner],
        &[],
    );
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![stranger, foreigner],
        "both arrive handle-less — the engine roster carries actor ids only"
    );

    let reader = Arc::new(ScriptedReader::new(vec![
        // 1. The read fails outright.
        None,
        // 2. A roster that predates both — it mentions neither.
        Some(vec![known(g.alice.identity_actor_id(), Some("alice"))]),
        // 3. The roster catches up: the stranger is local and named, the
        //    foreigner is listed with no handle (homed elsewhere).
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(stranger, Some("frank")),
            known(foreigner, None),
        ]),
        // 5. The foreigner's home nest has announced by now: the roster
        //    names them on the re-ask.
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(stranger, Some("frank")),
            known(foreigner, Some("fiona")),
        ]),
    ]));
    bob.backend.set_room_roster_reader(reader.clone());

    // 1. A failed read changes nothing and records nothing.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(reader.calls(), 1);
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![stranger, foreigner],
        "a read that failed must not be mistaken for an answer"
    );

    // 2. A roster that mentioned neither must not mark either answered.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(reader.calls(), 2);
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![stranger, foreigner],
        "an omission is not the answer 'no handle' — the report simply had \
         not reached the nest yet"
    );

    // 3. The roster catches up. The named member is seated as the canonical
    //    `handle@domain`; the listed-but-handle-less one keeps its elided id.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(reader.calls(), 3);
    let detail = bob.manager.thread_detail(bob.tid.clone()).unwrap();
    let handle_of = |actor: ActorId| {
        detail
            .participants
            .iter()
            .find(|p| p.person_actor_id() == Some(actor))
            .and_then(|p| p.person_handle())
            .map(str::to_string)
    };
    assert_eq!(
        handle_of(stranger).as_deref(),
        Some("frank@nest.test"),
        "a member never met is named, in the canonical handle@domain form a \
         typed recipient resolves to"
    );
    assert_eq!(
        handle_of(foreigner),
        None,
        "and one the roster listed without a handle stays elided"
    );
    assert_eq!(
        detail.participant_displays,
        detail
            .participants
            .iter()
            .map(|p| p.display())
            .collect::<Vec<_>>(),
        "the display column follows"
    );

    // 4. Both are now answered — the stranger by name for good, the foreigner
    //    by a listed absence that buys one poll of quiet — so this poll costs
    //    no read at all.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(
        reader.calls(),
        3,
        "a member the roster answered about is not asked again on the very \
         next poll — the read is bounded to membership events, not to the \
         poll tick"
    );

    // 5. The gap has passed: the foreigner is asked about again, and this
    //    time its home nest's announce has landed on the room's home, so it
    //    is named in place like the stranger was.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(
        reader.calls(),
        4,
        "a member listed WITHOUT a name is asked about again after the gap — \
         its name rides its own next drain, so 'nameless' is 'not yet'"
    );
    let detail = bob.manager.thread_detail(bob.tid.clone()).unwrap();
    let handle_of = |actor: ActorId| {
        detail
            .participants
            .iter()
            .find(|p| p.person_actor_id() == Some(actor))
            .and_then(|p| p.person_handle())
            .map(str::to_string)
    };
    assert_eq!(
        handle_of(foreigner).as_deref(),
        Some("fiona@nest.test"),
        "the foreign member is named once its home nest's announce reaches \
         the room's home — in the same canonical handle@domain form"
    );
    assert!(
        bob.manager.nameless_participants(&bob.tid).is_empty(),
        "nobody is elided any more"
    );

    // 6. Everybody is named, so there is nothing left to ask about.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(reader.calls(), 4, "a fully-named room costs no reads");
}

/// **The roster read is routed by the channel's recorded home** — the seam's
/// same-nest-vs-relay pick, made here in the backend where the home is known
/// (`conversation-rooms.md` § The home nest: "a member on a foreign nest
/// reaches the room only through their own home nest").
///
/// A room's floor roster lives on its home nest alone, so a foreign-homed
/// channel must carry its home URL through the seam — the glue then rides the
/// distinct kind `room.list_roster_remote`. Sending `None` there instead is
/// not a slower answer but a wrong one: the member's own nest holds no room
/// record and refuses the read, so every co-member stays elided forever. This
/// pins the argument the backend passes, in both directions.
#[tokio::test]
async fn the_roster_read_is_routed_by_the_channels_recorded_home() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let foreigner = ActorId([88u8; 32]);
    bob.manager.apply_inbound_roster(
        bob.tid.clone(),
        &[bob.actor, g.alice.identity_actor_id(), foreigner],
        &[],
    );
    // Every read lists the foreigner without a name, so the member stays
    // pending and the next poll asks again.
    let nameless = || {
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(foreigner, None),
        ])
    };
    let reader = Arc::new(ScriptedReader::new((0..4).map(|_| nameless()).collect()));
    bob.backend.set_room_roster_reader(reader.clone());

    // A channel with no recorded home is same-nest: no URL.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(
        reader.homes(),
        vec![None],
        "an unmarked channel reads its own nest's floor, as it always did"
    );

    // Record a foreign home — the same signal that routes this channel's
    // `channel.fetch` and `channel.actors` relays — and the roster read
    // follows it. (Poll 3 is the next ask: the gap after a listed-but-
    // nameless answer is one poll.)
    bob.backend
        .record_channel_home(g.channel_id, "https://home.example");
    for _ in 0..2 {
        bob.backend
            .resolve_nameless_members(&bob.manager, &g.channel_id)
            .await;
    }
    assert_eq!(
        reader.homes(),
        vec![None, Some("https://home.example".to_string())],
        "a foreign-homed channel carries its home URL through the seam, which \
         is what routes the read to the room's home instead of this nest"
    );

    // And back: an explicitly same-nest channel reports no URL again.
    bob.backend.record_channel_home(g.channel_id, "");
    for _ in 0..3 {
        bob.backend
            .resolve_nameless_members(&bob.manager, &g.channel_id)
            .await;
    }
    assert_eq!(
        reader.homes(),
        vec![None, Some("https://home.example".to_string()), None],
        "the pick is read fresh on every call, never cached from an earlier one"
    );
}

/// **A member the roster keeps listing without a name is re-asked at a gap
/// that doubles per nameless answer, capped** — so a foreign member whose
/// home nest never announces costs one read per
/// `NAMELESS_REASK_CAP` polls in the steady state, not one per poll, while
/// one whose first drain comes late is still named within a bounded number
/// of polls after it. The gap is a poll COUNT, never a wall-clock interval
/// (`e2e-conventions.md` convention 14).
#[tokio::test]
async fn a_listed_nameless_member_is_reasked_at_a_widening_gap_of_polls() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let foreigner = ActorId([88u8; 32]);
    bob.manager.apply_inbound_roster(
        bob.tid.clone(),
        &[bob.actor, g.alice.identity_actor_id(), foreigner],
        &[],
    );
    // Every read lists the foreigner without a name.
    let nameless = || {
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(foreigner, None),
        ])
    };
    let reader = Arc::new(ScriptedReader::new((0..8).map(|_| nameless()).collect()));
    bob.backend.set_room_roster_reader(reader.clone());

    // Poll 1 asks (call 1). Then gaps of 1, 2, 4, 8 polls: the next asks land
    // on polls 3, 6, 11, 20 — calls 2, 3, 4, 5.
    let mut expected_calls_after_poll = vec![];
    for poll in 1..=20u32 {
        bob.backend
            .resolve_nameless_members(&bob.manager, &g.channel_id)
            .await;
        expected_calls_after_poll.push((poll, reader.calls()));
    }
    let asked_on: Vec<u32> = expected_calls_after_poll
        .windows(2)
        .filter(|w| w[1].1 > w[0].1)
        .map(|w| w[1].0)
        .collect();
    assert_eq!(reader.calls(), 5, "five reads over twenty polls");
    assert_eq!(
        asked_on,
        vec![3, 6, 11, 20],
        "after the first ask on poll 1, the re-asks land after gaps of \
         1, 2, 4 and 8 polls — doubling per nameless answer"
    );
    assert_eq!(
        FaunaMlsBackend::NAMELESS_REASK_CAP,
        32,
        "the gap caps at 32 polls: one read per 32 polls is the steady-state \
         cost of a member whose home nest never announces"
    );
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![foreigner],
        "and through all of it the member stays honestly elided, never blank"
    );
}

/// **An actor the roster never lists at all is asked again for free on the
/// very next drain — the doubling gap only starts from the SECOND consecutive
/// omission**.
///
/// The committing device usually reports within seconds of its own commit,
/// so a member the floor omits on one drain is very likely to be listed on
/// the next — paying a gap for the first miss would slow the common case for
/// no reason. Only once an actor has been omitted twice in a row is a stalled
/// report (a home that never reports, or a report this device
/// simply has not polled yet) the more likely story, and the read backs off
/// at the same doubling gap a listed-but-nameless answer uses, capped at
/// [`FaunaMlsBackend::NAMELESS_REASK_CAP`].
#[tokio::test]
async fn an_omitted_member_is_reasked_free_once_then_at_a_widening_gap() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let foreigner = ActorId([88u8; 32]);
    bob.manager.apply_inbound_roster(
        bob.tid.clone(),
        &[bob.actor, g.alice.identity_actor_id(), foreigner],
        &[],
    );
    // Every read succeeds but never mentions the foreigner at all.
    let omits = || Some(vec![known(g.alice.identity_actor_id(), Some("alice"))]);
    let reader = Arc::new(ScriptedReader::new((0..15).map(|_| omits()).collect()));
    bob.backend.set_room_roster_reader(reader.clone());

    let mut asked_on = vec![];
    let mut last = 0;
    for poll in 1..=15u32 {
        bob.backend
            .resolve_nameless_members(&bob.manager, &g.channel_id)
            .await;
        let calls = reader.calls();
        if calls > last {
            asked_on.push(poll);
            last = calls;
        }
    }
    assert_eq!(
        asked_on,
        vec![1, 2, 4, 7, 12],
        "poll 1 asks (never asked before); poll 2 asks again for free (first \
         omission); only from there does the gap double — 1, 2, 4 polls — \
         same shape as the listed-but-nameless case, one drain later"
    );
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![foreigner],
        "never listed, never named — still honestly elided"
    );
}

/// **Any advanced commit on the channel clears its omission gap — not only
/// one that happens to re-seat the omitted actor**.
///
/// The obvious place to reset — the reconcile's add arm, which seats a
/// roster actor this device does not yet render — never fires for an
/// already-seated member: it skips every actor already in the participant
/// list (`ConversationsManager::apply_inbound_roster`'s `seated.contains`
/// guard), which is exactly the state an omitted member is in after their
/// first appearance. So the reset has to live where every advanced commit
/// passes through regardless of what it changed —
/// `backends::fauna_mls::reconcile_roster` — because the committing device's
/// own report typically lands moments after ITS commit, and a fresh commit
/// on the channel, whoever authored it, is exactly the signal that a stale
/// omission gap should not be trusted to still reflect the room.
#[tokio::test]
async fn an_advanced_commit_clears_the_channels_omission_gap() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );

    // Alice's appointment of bob is bob's first advanced commit to poll in.
    // The reconcile it drives seats carol — a real group member bob's
    // manager did not yet render — handle-less.
    alice
        .manager
        .appoint_admin(alice.tid.clone(), fauna_addr("bob", bob.actor))
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner may appoint"
    );
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob.backend, &bob.manager, &g.channel_id, &mut after_seq, 0)
        .await
        .unwrap();
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![g.carol.identity_actor_id()],
        "the reconcile seats every real group member; carol arrives handle-less"
    );

    // Three actual reads, all omitting carol, open a gap not yet spent when
    // the second commit lands: misses 1 (free), 2 (skip 1, spent by poll 3),
    // 3 (skip 2, unspent). The reader is set only ONCE — `room_roster_reader`
    // is a `OnceLock` (`set_room_roster_reader`), so a later re-set would be
    // a silent no-op — with a fourth, final answer queued for whichever poll
    // the gap (or the reset below) finally lets through.
    let omits = || Some(vec![known(g.alice.identity_actor_id(), Some("alice"))]);
    let named = || {
        Some(vec![
            known(g.alice.identity_actor_id(), Some("alice")),
            known(g.carol.identity_actor_id(), Some("carol")),
        ])
    };
    let reader = Arc::new(ScriptedReader::new(vec![
        omits(),
        omits(),
        omits(),
        named(),
    ]));
    bob.backend.set_room_roster_reader(reader.clone());
    for _ in 0..4 {
        bob.backend
            .resolve_nameless_members(&bob.manager, &g.channel_id)
            .await;
    }
    assert_eq!(
        reader.calls(),
        3,
        "poll 1 asks, poll 2 asks for free (first omission), poll 3 lands \
         inside the gap the second omission opened and asks nothing, poll 4 \
         asks again and opens a wider (skip 2) gap the third omission has \
         not spent yet"
    );
    assert_eq!(
        bob.manager.nameless_participants(&bob.tid),
        vec![g.carol.identity_actor_id()],
        "still elided — the fourth scripted answer is still queued"
    );

    // A second advanced commit lands — unrelated to carol, and carol is
    // already seated, so the reconcile's add-arm seat loop does not even
    // glance at her. The reset has to fire anyway.
    alice
        .manager
        .demote_admin(alice.tid.clone(), fauna_addr("bob", bob.actor))
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner may demote"
    );
    poll_inbound_conv(&bob.backend, &bob.manager, &g.channel_id, &mut after_seq, 0)
        .await
        .unwrap();

    // Carol is asked about on the very next drain — the commit cleared the
    // skip-2 gap the earlier omissions had built up, and the fourth scripted
    // answer finally names her.
    bob.backend
        .resolve_nameless_members(&bob.manager, &g.channel_id)
        .await;
    assert_eq!(
        reader.calls(),
        4,
        "the advanced commit reset the gap: this poll asks rather than \
         sitting out the skip 2 it had built up"
    );
    assert!(
        bob.manager.nameless_participants(&bob.tid).is_empty(),
        "carol is named"
    );
}

#[tokio::test]
async fn an_admin_removes_a_member_and_the_committing_device_reports_the_roster() {
    let g = governed_room();
    let alice = alice_seat(&g);
    // The owner appoints bob — an owner-only policy change, one commit.
    alice
        .manager
        .appoint_admin(
            alice.tid.clone(),
            fauna_addr("bob", g.bob.identity_actor_id()),
        )
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner may appoint"
    );
    catch_up(&g.bob, &g.nest, &g.channel_id);
    catch_up(&g.carol, &g.nest, &g.channel_id);

    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    let detail = bob.manager.thread_detail(bob.tid.clone()).unwrap();
    assert_eq!(detail.room.as_ref().unwrap().my_role, Some(RoomRole::Admin));
    assert!(detail.capabilities.can_remove_members && !detail.capabilities.can_appoint_admins);

    // An admin may not demote an admin, nor remove the owner.
    bob.manager
        .demote_admin(bob.tid.clone(), fauna_addr("bob", bob.actor))
        .await;
    let (key, message) = page_error_message(&bob.manager);
    assert_eq!(key, "conversations.unified.error_set_room_policy");
    assert_eq!(message, send_errors::ROOM_ADMINS_OWNER_ONLY);
    bob.manager
        .remove_participant(
            bob.tid.clone(),
            fauna_addr("alice", g.alice.identity_actor_id()),
        )
        .await;
    let (_, message) = page_error_message(&bob.manager);
    assert_eq!(message, send_errors::ROOM_OWNER_NOT_REMOVABLE);

    // The admin removes carol, and the committing device reports the
    // resulting roster to the home nest (report, never guess).
    let reporter = Arc::new(RecordingReporter::default());
    bob.backend.set_room_roster_reporter(reporter.clone());
    let before = g.nest.sent_envelopes(&g.channel_id.to_string()).len();
    bob.manager
        .remove_participant(
            bob.tid.clone(),
            fauna_addr("carol", g.carol.identity_actor_id()),
        )
        .await;
    assert!(bob.manager.snapshot().error.is_none(), "an admin removes");
    assert_eq!(
        g.nest.sent_envelopes(&g.channel_id.to_string()).len(),
        before + 1,
        "one Remove commit"
    );
    let counts = bob.backend.roster_report_counts();
    assert_eq!(
        (counts.owed, counts.delivered, counts.no_reporter),
        (1, 1, 0)
    );
    let reports = reporter.reports.lock().unwrap();
    let report = &reports[0];
    assert_eq!(report.channel_hex, g.channel_id.to_string());
    assert_eq!(report.policy_version, Some(2));
    assert_eq!(
        report.commit_seq,
        Some((before + 1) as i64),
        "the report names the log position its Remove commit landed at — what \
         lets the home nest drop it if a later commit's report got there first"
    );
    assert_eq!(
        report.home_nest_url, None,
        "a same-nest room's report carries no home: the glue rides the plain kind"
    );
    let mut roles: Vec<(ActorId, Option<RoomRole>)> =
        report.members.iter().map(|m| (m.actor, m.role)).collect();
    roles.sort_by_key(|(a, _)| a.0);
    let mut expected = vec![
        (g.alice.identity_actor_id(), Some(RoomRole::Owner)),
        (g.bob.identity_actor_id(), Some(RoomRole::Admin)),
    ];
    expected.sort_by_key(|(a, _)| a.0);
    assert_eq!(roles, expected, "carol is gone; the roles are the policy's");

    // A policy change rewrites roles, and roles are part of the roster the
    // nest mirrors — so alice's appointment of bob owed a report too (tallied
    // and dropped here: this seat registered no reporter).
    let counts = alice.backend.roster_report_counts();
    assert_eq!((counts.owed, counts.no_reporter), (1, 1));
}

/// A [`RoomRosterReporter`] whose home nest took every report and applied none:
/// the floor already holds a report at `by`. That is the answer a POSITIONED
/// report — a commit's — can get; no nest answers it to an unpositioned one,
/// which the replace takes wholesale (`conversation-rooms.md` § The floor
/// roster).
struct SupersededReporter {
    by: Option<i64>,
}

#[async_trait]
impl RoomRosterReporter for SupersededReporter {
    async fn report(&self, _report: RoomRosterReport) -> RoomRosterReportOutcome {
        RoomRosterReportOutcome::Superseded { by: self.by }
    }
}

/// A report the home nest took but did not apply is **tallied**, never counted
/// as a plain success: `superseded` rises inside `delivered`
/// (`conversation-rooms.md` § The floor roster). Without the tally a device
/// cannot tell its own commit's report from the one that replaced it.
#[tokio::test]
async fn a_superseded_roster_report_is_tallied_inside_delivered() {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice
        .backend
        .set_room_roster_reporter(Arc::new(SupersededReporter { by: Some(7) }));

    // The owner removes carol: one membership commit, one report owed.
    alice
        .manager
        .remove_participant(
            alice.tid.clone(),
            fauna_addr("carol", g.carol.identity_actor_id()),
        )
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner removes"
    );

    assert_eq!(
        alice.backend.roster_report_counts(),
        fauna_conversations::backends::fauna_mls::RosterReportCounts {
            owed: 1,
            no_reporter: 0,
            delivered: 1,
            undelivered: 0,
            superseded: 1,
        },
        "a superseded report reached the home, so it is delivered — and it is \
         tallied as superseded, not silently counted as stored"
    );
}

#[tokio::test]
async fn a_rename_on_a_governed_room_is_a_policy_commit_peers_apply_as_the_label() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let carol = carol_seat(&g);

    // A member's rename is refused before any commit.
    assert!(
        !carol
            .manager
            .thread_detail(carol.tid.clone())
            .unwrap()
            .capabilities
            .supports_rename,
        "rename is greyed for a member of a governed room"
    );
    carol
        .manager
        .rename_thread(carol.tid.clone(), "mine".into())
        .await;
    let (key, message) = page_error_message(&carol.manager);
    assert_eq!(key, "conversations.unified.error_rename_thread");
    assert_eq!(message, send_errors::ROOM_POLICY_NOT_PERMITTED);
    assert!(g.nest.sent_envelopes(&g.channel_id.to_string()).is_empty());

    // The owner's rename rides a Commit (the policy's name field)…
    alice
        .manager
        .rename_thread(alice.tid.clone(), "The Room".into())
        .await;
    assert!(alice.manager.snapshot().error.is_none());
    let envelopes = g.nest.sent_envelopes(&g.channel_id.to_string());
    assert_eq!(envelopes.len(), 1);
    assert!(matches!(
        ChannelEnvelope::from_bytes(&envelopes[0]).unwrap(),
        ChannelEnvelope::Commit(_)
    ));

    // …which a peer's inbound poll applies as the thread label, off the
    // agreed group context.
    let mut after_seq = 0i64;
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok");
    let detail = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert_eq!(detail.label, "The Room");
    let policy = detail.room.as_ref().unwrap().policy.as_ref().unwrap();
    assert_eq!(policy.name.as_deref(), Some("The Room"));
    assert_eq!(policy.version, 2);
}

/// A policy commit that moves neither the roster nor the label still moves
/// what every member PAINTS — the roles, and the role-gated capabilities the
/// room projection derives on every read — so folding it must tick the
/// snapshot observers. An event-driven painter (linux's GTK render, the FFI
/// apps' observer callbacks) repaints only on a tick: without one, a seat
/// whose thread is already open keeps painting the room as it stood before
/// the commit — the owner of yesterday's policy door stays live after the
/// hand-over lands, which is how the room journeys' first run on linux found
/// this (tui repaints every frame, so it never showed there).
#[tokio::test]
async fn a_policy_commit_that_moves_only_the_roles_ticks_the_observers() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let g = governed_room();
    let alice = alice_seat(&g);
    let carol = carol_seat(&g);

    // The owner appoints bob admin: a policy commit, roster and name untouched.
    alice
        .manager
        .appoint_admin(
            alice.tid.clone(),
            fauna_addr("bob", g.bob.identity_actor_id()),
        )
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner may appoint: {:?}",
        alice.manager.snapshot().error
    );

    struct Ticks(AtomicUsize);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let ticks = Arc::new(Ticks(AtomicUsize::new(0)));
    carol.manager.add_observer(ticks.clone());
    let before = carol.manager.thread_detail(carol.tid.clone()).unwrap();

    // Carol folds the appointment she did not author.
    let mut after_seq = 0i64;
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok");

    let after = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert_eq!(
        after.participants, before.participants,
        "the commit moved no member — the reconcile's own notify cannot be what ticks"
    );
    assert_eq!(after.label, before.label, "nor the name");
    let bob = after
        .participants
        .iter()
        .position(|p| p.person_actor_id() == Some(g.bob.identity_actor_id()))
        .expect("bob is seated on carol's thread");
    assert_eq!(
        after.room.as_ref().unwrap().members[bob].role,
        Some(RoomRole::Admin),
        "carol's projection reads bob as admin off the agreed group context"
    );
    assert!(
        ticks.0.load(Ordering::SeqCst) > 0,
        "folding a commit that changed what the room projects must tick the \
         observers, or every event-driven app keeps painting the old roles"
    );
}

/// The reconcile's **add arm** at the backend seam
/// (`conversation-rooms.md` § The floor roster: the roster every member
/// renders is the one the MLS group agrees on, never what this device last did
/// itself). The owner adds a fourth member; carol, who authored nothing, folds
/// that Add through `poll_inbound_conv` and seats him from the agreed roster
/// alone — handle-less, because the engine roster carries actor ids and
/// nothing else, and never seating carol herself.
#[tokio::test]
async fn a_peer_folds_an_add_it_did_not_author_onto_its_own_roster() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let carol = carol_seat(&g);

    let ids = |seat: &Seat| -> Vec<ActorId> {
        seat.manager
            .thread_detail(seat.tid.clone())
            .unwrap()
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect()
    };
    assert_eq!(
        ids(&carol),
        vec![g.alice.identity_actor_id(), g.bob.identity_actor_id()],
        "carol starts seated with the other two"
    );

    // A fourth engine, its key package on the nest for the owner's invite.
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    g.nest.seed_keypackage(
        &hex::encode(dave.identity_actor_id().0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    alice
        .backend
        .add_participant(
            alice.tid.clone(),
            fauna_addr("dave", dave.identity_actor_id()),
        )
        .await
        .expect("the owner may invite");

    // Carol folds the Add she did not author.
    let mut after_seq = 0i64;
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok");

    assert_eq!(
        ids(&carol),
        vec![
            g.alice.identity_actor_id(),
            g.bob.identity_actor_id(),
            dave.identity_actor_id(),
        ],
        "the member the owner added joins carol's list, at the end"
    );
    let detail = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert!(
        !ids(&carol).contains(&carol.actor),
        "carol is never her own participant row"
    );
    assert_eq!(
        detail.participants[2].person_handle(),
        None,
        "seated handle-less off the engine roster, and carol has never met dave"
    );
    assert_eq!(
        detail.participants[2].display(),
        fauna_core::format::short_id(&dave.identity_actor_id().to_hex()),
        "an unresolved member reads as its elided actor id, never as a blank row \
         (`value-formatting.md` § Account display label)"
    );
    assert_eq!(
        detail.participant_displays,
        detail
            .participants
            .iter()
            .map(|p| p.display())
            .collect::<Vec<_>>(),
        "the display column follows the participants"
    );
}

/// The reconcile's **drop arm** walks a predecessor's WHOLE forward chain, not
/// only its resolved terminal identity
/// (`ConversationsManager::apply_inbound_roster`;
/// `RoomPolicyExtension::successor_chain`).
/// Production records each hop's succession commit BEFORE that hop's
/// successor joins
/// (`fauna_client_recovery::group_sweep`, step 0 then step 1: record,
/// add-successor, remove-old), so a two-hop chain A→B→C has a live
/// intermediate holder (B) seated while the chain's terminal (C) has not
/// joined yet — resolving straight to the terminal drops A's row one commit
/// early, at the SECOND hop's record commit, before C has even been added.
/// Carol never authors anything here — a pure observer — so every commit
/// reaches her as a foreign one and reconcile fires on each in turn.
#[tokio::test]
async fn reconcile_retains_a_predecessor_through_a_two_hop_chain_while_any_hop_is_seated() {
    let g = governed_room();
    let carol = carol_seat(&g);
    let bob_actor = g.bob.identity_actor_id();

    let ids = |seat: &Seat| -> Vec<ActorId> {
        seat.manager
            .thread_detail(seat.tid.clone())
            .unwrap()
            .participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect()
    };
    assert!(ids(&carol).contains(&bob_actor), "bob starts seated");

    let mut after_seq = 0i64;

    // ── hop 1: bob (A) succeeds to a fresh identity B ───────────────────────
    let b_engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let b_actor = b_engine.identity_actor_id();

    // step 0: the OLD leaf (bob) records the succession first — agreed state
    // by the time any member judges the add (`group_sweep.rs` step 0).
    let policy = g.bob.room_policy(&g.channel_id).unwrap().unwrap();
    let recorded = policy
        .with_succession(RecordedSuccession {
            old: bob_actor,
            new: b_actor,
        })
        .unwrap();
    let record1 = g
        .bob
        .set_room_policy_staged(&g.channel_id, &recorded)
        .unwrap();
    g.bob.merge_pending_commit(&g.channel_id).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(record1).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .expect("bob's record commit sits on the log");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds bob's record commit");
    assert!(
        ids(&carol).contains(&bob_actor),
        "recording alone changes nothing: bob is still literally seated"
    );

    // step 1: bob commits add-successor(B); B joins in-process from the Welcome.
    let b_kp = b_engine
        .generate_key_packages(1)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let add1 = commit_add_successor(&g.bob, &g.channel_id, &b_kp).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(add1.commit_bytes)
                .to_bytes()
                .unwrap(),
            None,
            vec![],
        )
        .await
        .expect("bob's add-successor commit sits on the log");
    b_engine.join_from_welcome(add1.welcome).unwrap();
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds bob's add-successor commit");
    assert_eq!(
        ids(&carol).iter().filter(|a| **a == bob_actor).count(),
        1,
        "bob's row still stands for the pair"
    );
    assert!(
        !ids(&carol).contains(&b_actor),
        "B stands in for bob's row, not a newcomer"
    );

    // step 2: the NEW leaf (B) commits remove-old(bob).
    let remove1 = commit_remove_old(&b_engine, &g.channel_id, &bob_actor).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(remove1).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .expect("B's remove-old commit sits on the log");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds B's remove-old commit");
    assert!(
        ids(&carol).contains(&bob_actor),
        "bob's row is retained: its immediate successor B is seated"
    );

    // ── hop 2: B succeeds to a second fresh identity C. Neither hop's
    // statement is ever authored or verified in this test, so the row's
    // identity stays "bob" throughout — only the RecordedSuccession chain
    // moves, exactly the parked-statement shape the retention exists for.
    let c_engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let c_actor = c_engine.identity_actor_id();

    let policy2 = b_engine.room_policy(&g.channel_id).unwrap().unwrap();
    let recorded2 = policy2
        .with_succession(RecordedSuccession {
            old: b_actor,
            new: c_actor,
        })
        .unwrap();
    let record2 = b_engine
        .set_room_policy_staged(&g.channel_id, &recorded2)
        .unwrap();
    b_engine.merge_pending_commit(&g.channel_id).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(record2).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .expect("B's record commit sits on the log");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds B's record commit");
    assert!(
        ids(&carol).contains(&bob_actor),
        "bob's row must survive the SECOND hop's record commit too: B, its live \
         immediate successor, is still seated even though the chain now resolves \
         to the not-yet-joined C"
    );

    let c_kp = c_engine
        .generate_key_packages(1)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let add2 = commit_add_successor(&b_engine, &g.channel_id, &c_kp).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(add2.commit_bytes)
                .to_bytes()
                .unwrap(),
            None,
            vec![],
        )
        .await
        .expect("B's add-successor commit sits on the log");
    c_engine.join_from_welcome(add2.welcome).unwrap();
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds B's add-successor commit");
    assert!(
        ids(&carol).contains(&bob_actor),
        "bob's row still stands mid second hop"
    );
    assert!(
        !ids(&carol).contains(&c_actor),
        "C stands in for bob's row too, not a newcomer"
    );

    let remove2 = commit_remove_old(&c_engine, &g.channel_id, &b_actor).unwrap();
    g.nest
        .channel_send(
            g.channel_id.to_string(),
            ChannelEnvelope::Commit(remove2).to_bytes().unwrap(),
            None,
            vec![],
        )
        .await
        .expect("C's remove-old commit sits on the log");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds C's remove-old commit");
    assert!(
        ids(&carol).contains(&bob_actor),
        "bob's row survives to the terminal: C, the chain's live holder, is seated"
    );

    // ── C leaves too, with no further succession recorded — bob's row must
    // finally drop, since nothing on its chain is seated any more. Only the
    // owner may author a plain (non-predecessor) removal, so alice does it —
    // catching her own engine up on the whole log first, since she has
    // authored and folded nothing above.
    let alice = alice_seat(&g);
    let mut alice_seq = 0i64;
    poll_inbound_conv(
        &alice.backend,
        &alice.manager,
        &g.channel_id,
        &mut alice_seq,
        0,
    )
    .await
    .expect("alice catches up to the same state");
    alice
        .backend
        .remove_participant(alice.tid.clone(), fauna_addr("c", c_actor))
        .await
        .expect("the owner may remove a plain member");
    poll_inbound_conv(
        &carol.backend,
        &carol.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("carol folds alice's removal of C");
    assert!(
        !ids(&carol).contains(&bob_actor),
        "bob's row finally drops: no identity on its chain is seated any more"
    );
}

/// The outgoing owner is **told** its hand-over was refused, instead of
/// inferring it from the roles never changing (`conversation-rooms.md`
/// § Roles and authorization → *Ownership transfer*: an offer the room has
/// moved past is refused by every member and dropped by the device holding it
/// — "the owner offers again"). The drop happens on the *incoming* owner's
/// device, the only one that parks the offer, so the seat that must act on it
/// is the one seat that held no record of it.
///
/// Alice (owner) appoints bob admin, then offers the room to carol. Bob's
/// admin-level policy change takes the version alice's offer was going to
/// claim; alice folds bob's commit and her page says so.
#[tokio::test]
async fn an_offer_the_room_moved_past_tells_the_owner_who_made_it() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let bob_addr = fauna_addr("bob", g.bob.identity_actor_id());
    let carol_addr = fauna_addr("carol", g.carol.identity_actor_id());

    // The owner appoints bob admin (version 1 -> 2), then offers the room to
    // carol -- the offer would be version 3, and rides the channel as an
    // application message, not a commit.
    alice
        .manager
        .appoint_admin(alice.tid.clone(), bob_addr.clone())
        .await;
    assert!(alice.manager.snapshot().error.is_none());
    alice
        .manager
        .transfer_room_ownership(alice.tid.clone(), carol_addr)
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the offer itself posts cleanly: {:?}",
        alice.manager.snapshot().error
    );

    // Bob catches up and, as an admin, changes the join rule -- the version
    // alice's outstanding offer needed.
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    let mut bob_seq = 0i64;
    poll_inbound_conv(&bob.backend, &bob.manager, &g.channel_id, &mut bob_seq, 0)
        .await
        .expect("bob catches up");
    bob.manager
        .set_room_join_rule(bob.tid.clone(), JoinRule::MemberInvite)
        .await;
    assert!(
        bob.manager.snapshot().error.is_none(),
        "an admin may set the join rule: {:?}",
        bob.manager.snapshot().error
    );

    // Alice folds bob's commit. Her offer can never be the next version now,
    // and her page says so rather than leaving her watching roles that will
    // not change.
    let mut alice_seq = 0i64;
    poll_inbound_conv(
        &alice.backend,
        &alice.manager,
        &g.channel_id,
        &mut alice_seq,
        0,
    )
    .await
    .expect("alice folds");

    let (key, message) = page_error_message(&alice.manager);
    assert_eq!(key, "conversations.unified.error_set_room_policy");
    assert_eq!(message, send_errors::ROOM_TRANSFER_SUPERSEDED);
    let detail = alice.manager.thread_detail(alice.tid.clone()).unwrap();
    assert_eq!(
        detail.room.as_ref().unwrap().my_role,
        Some(RoomRole::Owner),
        "the room did not change hands"
    );
}

#[tokio::test]
async fn history_full_hands_the_newcomer_the_inviters_slice() {
    let g = governed_room();
    let alice = alice_seat(&g);
    // Two messages before the newcomer exists.
    for body in ["first", "second"] {
        alice
            .manager
            .set_compose_body(alice.tid.clone(), body.into());
        alice
            .manager
            .send(alice.tid.clone())
            .await
            .expect("send ok");
    }
    // The owner turns history on.
    alice
        .manager
        .set_room_history_policy(alice.tid.clone(), HistoryPolicy::Full)
        .await;
    assert!(alice.manager.snapshot().error.is_none());

    // Dave is invited.
    let dave = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let dave_actor = dave.identity_actor_id();
    g.nest.seed_keypackage(
        &hex::encode(dave_actor.0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let sends_before = g.nest.sent_envelopes(&g.channel_id.to_string()).len();
    alice.manager.open_add_participant(alice.tid.clone());
    alice
        .manager
        .accept_add_participant_chip(fauna_addr("dave", dave_actor));
    alice
        .manager
        .confirm_add_participant()
        .await
        .expect("add ok");
    assert!(alice.manager.snapshot().error.is_none());
    let envelopes = g.nest.sent_envelopes(&g.channel_id.to_string());
    assert_eq!(
        envelopes.len(),
        sends_before + 2,
        "the Add commit, then the history slice in the newcomer's first epoch"
    );
    assert!(matches!(
        ChannelEnvelope::from_bytes(envelopes.last().unwrap()).unwrap(),
        ChannelEnvelope::Application(_)
    ));

    // Dave joins and walks the log: the pre-join ciphertext is unreadable to
    // him, the slice is not — and it seeds his thread with the transcript.
    let welcome = g.nest.welcomes().pop().unwrap();
    dave.join_from_welcome_bytes(&welcome.welcome_bytes)
        .unwrap();
    let dave = seat(
        dave.clone(),
        &g.nest,
        "dave",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    );
    let mut after_seq = 0i64;
    poll_inbound_conv(
        &dave.backend,
        &dave.manager,
        &g.channel_id,
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok");
    let detail = dave.manager.thread_detail(dave.tid.clone()).unwrap();
    let bodies: Vec<String> = detail.messages.iter().map(|m| m.body.clone()).collect();
    assert_eq!(
        bodies,
        vec!["first", "second"],
        "the newcomer sees the past"
    );
}

#[tokio::test]
async fn history_none_hands_the_newcomer_nothing() {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice
        .manager
        .set_compose_body(alice.tid.clone(), "before".into());
    alice.manager.send(alice.tid.clone()).await.unwrap();

    let dave = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let dave_actor = dave.identity_actor_id();
    g.nest.seed_keypackage(
        &hex::encode(dave_actor.0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let sends_before = g.nest.sent_envelopes(&g.channel_id.to_string()).len();
    alice.manager.open_add_participant(alice.tid.clone());
    alice
        .manager
        .accept_add_participant_chip(fauna_addr("dave", dave_actor));
    alice.manager.confirm_add_participant().await.unwrap();
    assert_eq!(
        g.nest.sent_envelopes(&g.channel_id.to_string()).len(),
        sends_before + 1,
        "the Add commit alone — no slice under `none`"
    );
    // And the rail itself refuses a slice the policy does not authorize.
    let slice = alice
        .manager
        .snapshot_channel_slice(&alice.tid, &g.channel_id.to_string(), 0)
        .unwrap();
    assert!(
        alice
            .backend
            .deliver_history_slice(alice.tid.clone(), &slice)
            .await
            .is_err()
    );
}

// ── The receiving half of history for joiners ─────────────────────────────
//
// `conversation-rooms.md` § History for joiners → *What a device accepts*. A
// `HistorySlice` is an application message, so MLS lets ANY member seal one;
// what makes one the newcomer's history is decided on the receiving device.
// The hostile member below is a patched client: it seals whatever slice it
// likes through the raw engine (`post_raw`), past every producer-side refusal.

/// `text` as `sender` said it under `id`, shaped off a real message so the
/// snapshot is one a slice could honestly carry.
fn carried(
    template: &fauna_conversations::message::MessageSnapshot,
    id: String,
    sender: TypedAddress,
    text: &str,
) -> fauna_conversations::message::MessageSnapshot {
    let mut m = template.clone();
    m.message_id = MessageId(id);
    m.sender_display = sender.display();
    m.sender = sender;
    m.body = text.to_string();
    m.is_own = false;
    m
}

/// The first message `seat` holds on its thread — the template [`carried`]
/// shapes a slice entry from.
fn first_held(seat: &Seat) -> fauna_conversations::message::MessageSnapshot {
    seat.manager
        .thread_detail(seat.tid.clone())
        .unwrap()
        .messages
        .first()
        .expect("the seat holds a message")
        .clone()
}

fn bodies(seat: &Seat) -> Vec<String> {
    seat.manager
        .thread_detail(seat.tid.clone())
        .unwrap()
        .messages
        .iter()
        .map(|m| m.body.clone())
        .collect()
}

async fn poll_all(seat: &Seat, channel: &ChannelId) {
    let mut after_seq = 0i64;
    poll_inbound_conv(&seat.backend, &seat.manager, channel, &mut after_seq, 0)
        .await
        .expect("poll ok");
}

async fn post_slice(g: &Governed, engine: &MlsEngine, slice: &ChannelHistorySlice) {
    post_raw(
        g,
        engine,
        ChannelMessageBody::GroupMeta(GroupMetaMessage::HistorySlice(slice.to_bytes().unwrap())),
    )
    .await;
}

/// Alice's governed room with one message said ("old", seq 1), the history
/// policy set by the caller, and dave admitted by alice's **bare Add** — the
/// backend's, which posts the commit and the Welcome and no slice, so each
/// test seals the slice it is about. Returns the room, alice's seat, dave's
/// joined engine and the real message a slice entry is shaped from.
async fn room_with_dave_admitted(
    history: HistoryPolicy,
) -> (
    Governed,
    Seat,
    Arc<MlsEngine>,
    fauna_conversations::message::MessageSnapshot,
) {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice
        .manager
        .set_compose_body(alice.tid.clone(), "old".into());
    alice
        .manager
        .send(alice.tid.clone())
        .await
        .expect("send ok");
    if history == HistoryPolicy::Full {
        alice
            .manager
            .set_room_history_policy(alice.tid.clone(), HistoryPolicy::Full)
            .await;
        assert!(alice.manager.snapshot().error.is_none());
    }
    let dave = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    g.nest.seed_keypackage(
        &hex::encode(dave.identity_actor_id().0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    alice
        .backend
        .add_participant(
            alice.tid.clone(),
            fauna_addr("dave", dave.identity_actor_id()),
        )
        .await
        .expect("add ok");
    let welcome = g.nest.welcomes().pop().unwrap();
    dave.join_from_welcome_bytes(&welcome.welcome_bytes)
        .unwrap();
    let template = first_held(&alice);
    (g, alice, dave, template)
}

fn dave_seat(g: &Governed, dave: &Arc<MlsEngine>) -> Seat {
    seat(
        dave.clone(),
        &g.nest,
        "dave",
        g.channel_id,
        vec![fauna_addr("alice", g.alice.identity_actor_id())],
    )
}

/// Arm (a) of the finding: a member who admitted nobody seals a slice carrying
/// another member's NEXT message id. Every other member used to fold it, the
/// pre-decrypt skip then met the forged copy under that id, and the real
/// message never rendered.
#[tokio::test]
async fn a_members_history_slice_cannot_forge_a_future_message() {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice
        .manager
        .set_compose_body(alice.tid.clone(), "seed".into());
    alice
        .manager
        .send(alice.tid.clone())
        .await
        .expect("send ok"); // seq 1
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());

    let mut slice = empty_channel_slice(&ch, "renamed by bob", alice_addr.clone());
    slice.messages = vec![carried(
        &first_held(&alice),
        format!("conv:{ch}:3"),
        alice_addr,
        "FORGED",
    )];
    post_slice(&g, &g.bob, &slice).await; // seq 2

    alice
        .manager
        .set_compose_body(alice.tid.clone(), "hello".into());
    alice
        .manager
        .send(alice.tid.clone())
        .await
        .expect("send ok"); // seq 3

    let carol = carol_seat(&g);
    let label_before = carol
        .manager
        .thread_detail(carol.tid.clone())
        .unwrap()
        .label;
    poll_all(&carol, &g.channel_id).await;
    assert_eq!(
        bodies(&carol),
        vec!["seed", "hello"],
        "the real message renders, and nothing the slice carried does"
    );
    assert_eq!(
        carol
            .manager
            .thread_detail(carol.tid.clone())
            .unwrap()
            .label,
        label_before,
        "a member's slice renames nothing"
    );
}

/// Arm (b): carol sits in room A with bob and in room B without him. Bob's
/// slice in A names B's next message id; B's real record must still fold.
#[tokio::test]
async fn a_history_slice_in_one_room_cannot_suppress_another_rooms_record() {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice
        .manager
        .set_compose_body(alice.tid.clone(), "seed".into());
    alice
        .manager
        .send(alice.tid.clone())
        .await
        .expect("send ok");

    // Room B: erin and carol, on the same nest. Bob is not in it.
    let erin = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let (room_b, welcome_b) = erin
        .create_group(&g.carol.generate_key_packages(1).unwrap())
        .unwrap();
    g.carol.join_from_welcome(welcome_b).unwrap();
    let b_hex = room_b.to_string();

    let carol = carol_seat(&g);
    let erin_addr = fauna_addr("erin", erin.identity_actor_id());
    let tid_b = carol
        .manager
        .materialize_conv_thread(b_hex.clone(), vec![erin_addr.clone()]);
    carol.backend.bind_channel(tid_b.clone(), room_b);

    let mut slice = empty_channel_slice(
        &g.channel_id.to_string(),
        "thread",
        fauna_addr("alice", g.alice.identity_actor_id()),
    );
    slice.messages = vec![carried(
        &first_held(&alice),
        format!("conv:{b_hex}:1"),
        erin_addr,
        "SQUATTER",
    )];
    post_slice(&g, &g.bob, &slice).await;
    poll_all(&carol, &g.channel_id).await;

    // Erin's first real message in B.
    let message = ChannelMessage {
        sender: erin.identity_actor_id(),
        sequence: 1,
        channel_epoch: 0,
        body: ChannelMessageBody::Text("in B".into()),
        timestamp: Timestamp::now(),
    };
    g.nest
        .channel_send(
            b_hex.clone(),
            ChannelEnvelope::Application(erin.encrypt(&room_b, &message).unwrap())
                .to_bytes()
                .unwrap(),
            None,
            vec![],
        )
        .await
        .unwrap();
    let mut after_seq = 0i64;
    poll_inbound_conv(&carol.backend, &carol.manager, &room_b, &mut after_seq, 0)
        .await
        .expect("poll ok");
    let in_b: Vec<String> = carol
        .manager
        .thread_detail(tid_b)
        .unwrap()
        .messages
        .iter()
        .map(|m| m.body.clone())
        .collect();
    assert_eq!(in_b, vec!["in B"], "room B's record folds");
}

/// The skip that made arm (b) work, pinned on its own: the walk may skip a
/// record only when THIS channel's thread already holds its id. Reached here
/// through the one door that may still put a foreign id in a thread — the
/// user's own devices' replica restore — so the scope holds whoever wrote it.
#[tokio::test]
async fn a_message_id_held_by_another_thread_never_skips_a_channels_own_record() {
    let g = governed_room();
    let carol = carol_seat(&g);
    let alice = alice_seat(&g);
    alice
        .manager
        .set_compose_body(alice.tid.clone(), "seed".into());
    alice
        .manager
        .send(alice.tid.clone())
        .await
        .expect("send ok");
    let template = first_held(&alice);

    // Another thread on carol's device holds this channel's id for seq 1.
    let other_hex = hex::encode([7u8; 32]);
    let mut replica = empty_channel_slice(
        &other_hex,
        "other",
        fauna_addr("erin", ActorKeypair::generate().actor_id()),
    );
    replica.messages = vec![carried(
        &template,
        format!("conv:{}:1", g.channel_id),
        fauna_addr("erin", ActorKeypair::generate().actor_id()),
        "held elsewhere",
    )];
    carol.manager.restore_channel_slice(&replica);

    poll_all(&carol, &g.channel_id).await;
    assert_eq!(
        bodies(&carol),
        vec!["seed"],
        "the channel's own record folds"
    );
}

/// The inviter's own slice, in the newcomer's first epoch — refused because
/// the room's policy says a newcomer is owed nothing.
#[tokio::test]
async fn a_history_slice_under_none_is_refused() {
    let (g, _alice, dave, template) = room_with_dave_admitted(HistoryPolicy::None).await;
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());
    let mut slice = empty_channel_slice(&ch, "thread", alice_addr.clone());
    slice.messages = vec![carried(
        &template,
        format!("conv:{ch}:1"),
        alice_addr,
        "old",
    )];
    post_slice(&g, &g.alice, &slice).await;

    let dave = dave_seat(&g, &dave);
    poll_all(&dave, &g.channel_id).await;
    assert_eq!(bodies(&dave), Vec::<String>::new());
}

/// Under `full`, a member who did not admit the newcomer seals a slice beside
/// the inviter's — and beats it to the log. Only the inviter's is taken, and
/// the stranger's does not spend the newcomer's one admission.
#[tokio::test]
async fn a_history_slice_from_a_member_who_did_not_invite_is_refused() {
    let (g, _alice, dave, template) = room_with_dave_admitted(HistoryPolicy::Full).await;
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());

    catch_up(&g.bob, &g.nest, &g.channel_id);
    let mut bobs = empty_channel_slice(&ch, "thread", alice_addr.clone());
    bobs.messages = vec![carried(
        &template,
        format!("conv:{ch}:2"),
        alice_addr.clone(),
        "FROM BOB",
    )];
    post_slice(&g, &g.bob, &bobs).await;

    let mut alices = empty_channel_slice(&ch, "thread", alice_addr.clone());
    alices.messages = vec![carried(
        &template,
        format!("conv:{ch}:1"),
        alice_addr,
        "old",
    )];
    post_slice(&g, &g.alice, &alices).await;

    let dave = dave_seat(&g, &dave);
    poll_all(&dave, &g.channel_id).await;
    assert_eq!(bodies(&dave), vec!["old"]);
}

/// What the newcomer holds after the inviter's slice carried `bad_id` beside
/// one honest entry.
async fn bodies_after_inviter_slice_with(bad_id: Option<fn(&str) -> String>) -> Vec<String> {
    let (g, _alice, dave, template) = room_with_dave_admitted(HistoryPolicy::Full).await;
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());
    let mut slice = empty_channel_slice(&ch, "thread", alice_addr.clone());
    slice.messages = vec![carried(
        &template,
        format!("conv:{ch}:1"),
        alice_addr.clone(),
        "old",
    )];
    if let Some(bad_id) = bad_id {
        slice
            .messages
            .push(carried(&template, bad_id(&ch), alice_addr, "BAD"));
    }
    post_slice(&g, &g.alice, &slice).await;
    let dave = dave_seat(&g, &dave);
    poll_all(&dave, &g.channel_id).await;
    bodies(&dave)
}

/// Refused WHOLE, never filtered: a slice that names an id outside its own
/// channel's past is not the honest inviter's, so nothing in it is believed.
#[tokio::test]
async fn a_history_slice_carrying_a_foreign_or_future_id_is_refused_whole() {
    assert_eq!(
        bodies_after_inviter_slice_with(None).await,
        vec!["old"],
        "control: the same slice without the bad entry is taken"
    );
    let nothing = Vec::<String>::new();
    assert_eq!(
        bodies_after_inviter_slice_with(Some(|_| format!("conv:{}:1", hex::encode([9u8; 32]))))
            .await,
        nothing,
        "another channel's id"
    );
    assert_eq!(
        bodies_after_inviter_slice_with(Some(|ch| format!("conv:{ch}:999"))).await,
        nothing,
        "an id at or past the slice's own record"
    );
    assert_eq!(
        bodies_after_inviter_slice_with(Some(|ch| format!("mail:{ch}:1"))).await,
        nothing,
        "an id of another shape"
    );
}

/// The inviter's slice is taken for its messages alone. The room's name is
/// the owner-signed policy's, and whose handle an owner typed is the user's
/// own devices' datum — whether the member's slice records no owner-typed
/// row or names one.
#[tokio::test]
async fn an_inviters_history_slice_gives_messages_and_neither_a_name_nor_provenance() {
    for recorded in [false, true] {
        let (g, _alice, dave, template) = room_with_dave_admitted(HistoryPolicy::Full).await;
        let ch = g.channel_id.to_string();
        let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());
        let bob_actor = g.bob.identity_actor_id();
        let mut slice = empty_channel_slice(&ch, "named by the slice", alice_addr.clone());
        slice.participants.push(fauna_addr("bob", bob_actor));
        slice.anchor_grade_handles = if recorded { vec![bob_actor] } else { vec![] };
        slice.messages = vec![carried(
            &template,
            format!("conv:{ch}:1"),
            alice_addr,
            "old",
        )];
        post_slice(&g, &g.alice, &slice).await;

        let dave = seat(
            dave.clone(),
            &g.nest,
            "dave",
            g.channel_id,
            vec![
                fauna_addr("alice", g.alice.identity_actor_id()),
                fauna_addr("bob", bob_actor),
            ],
        );
        poll_all(&dave, &g.channel_id).await;
        assert_eq!(bodies(&dave), vec!["old"]);
        assert_ne!(
            dave.manager.thread_detail(dave.tid.clone()).unwrap().label,
            "named by the slice"
        );
        assert_eq!(
            dave.manager.anchor_grade_handle_for(&bob_actor),
            None,
            "a member's slice marks no row as owner-typed (bob recorded: {recorded})"
        );
    }
}

/// The admission is for the epoch the newcomer joined at. An inviter who holds
/// its slice back until the room has moved on — which is every long-standing
/// member's inviter, for ever — has nothing left to hand over.
#[tokio::test]
async fn an_inviters_history_slice_after_the_newcomers_first_epoch_is_refused() {
    let (g, alice, dave, template) = room_with_dave_admitted(HistoryPolicy::Full).await;
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());
    // A commit: the room leaves the epoch dave joined at.
    alice
        .manager
        .set_room_join_rule(alice.tid.clone(), JoinRule::MemberInvite)
        .await;
    assert!(alice.manager.snapshot().error.is_none());
    let mut slice = empty_channel_slice(&ch, "thread", alice_addr.clone());
    slice.messages = vec![carried(
        &template,
        format!("conv:{ch}:1"),
        alice_addr,
        "old",
    )];
    post_slice(&g, &g.alice, &slice).await;

    let dave = dave_seat(&g, &dave);
    poll_all(&dave, &g.channel_id).await;
    assert_eq!(bodies(&dave), Vec::<String>::new());
}

/// One slice per admission: the inviter's second slice — sealed whenever it
/// likes while the newcomer's first epoch lasts — is not history.
#[tokio::test]
async fn an_inviters_second_history_slice_is_refused() {
    let (g, _alice, dave, template) = room_with_dave_admitted(HistoryPolicy::Full).await;
    let ch = g.channel_id.to_string();
    let alice_addr = fauna_addr("alice", g.alice.identity_actor_id());
    for (seq, text) in [(1, "old"), (2, "LATER")] {
        let mut slice = empty_channel_slice(&ch, "thread", alice_addr.clone());
        slice.messages = vec![carried(
            &template,
            format!("conv:{ch}:{seq}"),
            alice_addr.clone(),
            text,
        )];
        post_slice(&g, &g.alice, &slice).await;
    }
    let dave = dave_seat(&g, &dave);
    poll_all(&dave, &g.channel_id).await;
    assert_eq!(bodies(&dave), vec!["old"]);
}

#[tokio::test]
async fn evict_person_everywhere_reports_a_room_policy_refusal_as_a_typed_seat() {
    let g = governed_room();
    let carol = carol_seat(&g);
    let outcome = carol
        .manager
        .evict_person_everywhere(&g.bob.identity_actor_id())
        .await;
    assert!(outcome.evicted.is_empty() && outcome.failed.is_empty());
    assert_eq!(outcome.unreachable.len(), 1);
    assert_eq!(
        outcome.unreachable[0].class,
        UnreachableSeatClass::NotPermittedByRoomPolicy
    );
    assert_eq!(outcome.unreachable[0].channel_hex, g.channel_id.to_string());
    assert!(
        !outcome.is_complete(),
        "the seat stands; `Removed` is not earned"
    );
    assert!(g.nest.sent_envelopes(&g.channel_id.to_string()).is_empty());

    // The owner's eviction of the same person goes through.
    let alice = alice_seat(&g);
    let outcome = alice
        .manager
        .evict_person_everywhere(&g.bob.identity_actor_id())
        .await;
    assert_eq!(outcome.evicted, vec![alice.tid.clone()]);
    assert!(outcome.is_complete());
}

#[tokio::test]
async fn member_invite_lets_a_member_invite_and_the_join_rule_is_an_owner_or_admin_edit() {
    let g = governed_room();
    let carol = carol_seat(&g);
    // A member cannot change the join rule.
    carol
        .manager
        .set_room_join_rule(carol.tid.clone(), JoinRule::MemberInvite)
        .await;
    let (key, message) = page_error_message(&carol.manager);
    assert_eq!(key, "conversations.unified.error_set_room_policy");
    assert_eq!(message, send_errors::ROOM_POLICY_NOT_PERMITTED);

    // The owner can, and the member's next projection says so.
    let alice = alice_seat(&g);
    alice
        .manager
        .set_room_join_rule(alice.tid.clone(), JoinRule::MemberInvite)
        .await;
    assert!(alice.manager.snapshot().error.is_none());
    catch_up(&g.carol, &g.nest, &g.channel_id);
    let detail = carol.manager.thread_detail(carol.tid.clone()).unwrap();
    assert!(
        detail.capabilities.can_invite,
        "member-invite opens the invite"
    );
    assert!(!detail.capabilities.can_remove_members, "…and nothing else");
    assert_eq!(
        detail
            .room
            .as_ref()
            .unwrap()
            .policy
            .as_ref()
            .unwrap()
            .join_rule,
        JoinRule::MemberInvite
    );

    // And carol's invite now reaches the wire.
    let dave = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    g.nest.seed_keypackage(
        &hex::encode(dave.identity_actor_id().0),
        dave.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    carol
        .backend
        .add_participant(
            carol.tid.clone(),
            fauna_addr("dave", dave.identity_actor_id()),
        )
        .await
        .expect("a member invites under member-invite");
    assert_eq!(g.nest.welcomes().len(), 1);
    let _ = RoomPolicyEdit::Rename(String::new());
}

// ── The community class: authored, sealed, read back attributed ────────

use fauna_conversations::backend::{GroupReceptionKeys, RoomGenerationReader, RoomGenerationWrap};
use fauna_core::crypto::GenerationKey;
use fauna_core::group_generation::{GroupReceptionKeyRecord, group_generation_key_commitment};
use fauna_mls::wrapped_blob::group_generation_wraps::seal_group_generation_key_to_entry;

/// A scripted [`RoomGenerationReader`] — the room's wraps as this seat's own
/// nest read would serve them, plus the `home_nest_url` it was asked with (the
/// same-nest-vs-relay pick the backend makes) and a call count, so a test can
/// assert the per-channel cache actually saves round trips.
struct ScriptedGenerations {
    wraps: Mutex<Vec<RoomGenerationWrap>>,
    calls: std::sync::atomic::AtomicUsize,
    homes: Mutex<Vec<Option<String>>>,
}

impl ScriptedGenerations {
    fn new(wraps: Vec<RoomGenerationWrap>) -> Self {
        Self {
            wraps: Mutex::new(wraps),
            calls: std::sync::atomic::AtomicUsize::new(0),
            homes: Mutex::new(Vec::new()),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl RoomGenerationReader for ScriptedGenerations {
    async fn read_generations(
        &self,
        _channel_hex: String,
        home_nest_url: Option<String>,
    ) -> Option<Vec<RoomGenerationWrap>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.homes.lock().unwrap().push(home_nest_url);
        Some(self.wraps.lock().unwrap().clone())
    }
}

/// The account-plane half: the reception keypairs this seat holds, and
/// whether a write to it lands.
///
/// `writable` is what lets a test drive the record-then-act rule: a founding
/// whose keypair cannot be persisted must abandon the seating rather than hand
/// a room a public half whose secret never reached disk.
struct HeldReceptionKeys {
    held: Mutex<Vec<GroupReceptionKeyRecord>>,
    writable: bool,
}

impl HeldReceptionKeys {
    /// A seat already holding these keys; writes land.
    fn holding(keys: Vec<GroupReceptionKeyRecord>) -> Arc<Self> {
        Arc::new(Self {
            held: Mutex::new(keys),
            writable: true,
        })
    }
    /// An account that holds none yet — the state before its first room.
    fn empty() -> Arc<Self> {
        Self::holding(Vec::new())
    }
    /// An account that holds none and whose store refuses every write.
    fn unwritable() -> Arc<Self> {
        Arc::new(Self {
            held: Mutex::new(Vec::new()),
            writable: false,
        })
    }
}

#[async_trait]
impl GroupReceptionKeys for HeldReceptionKeys {
    async fn reception_keys(&self) -> Vec<GroupReceptionKeyRecord> {
        self.held.lock().unwrap().clone()
    }

    async fn put_reception_key(&self, record: GroupReceptionKeyRecord) -> bool {
        if !self.writable {
            return false;
        }
        // Newest first, like the account runtime's own read.
        self.held.lock().unwrap().insert(0, record);
        true
    }
}

/// One seat of a community room: its own engine (deliberately holding **no MLS
/// group** — that is what the class is), its own reception keypair, and its own
/// roster entry, so the wrap it is served is genuinely addressed to it.
struct RoomSeat {
    engine: Arc<MlsEngine>,
    actor: ActorId,
    backend: Arc<FaunaMlsBackend>,
    manager: Arc<ConversationsManager>,
    thread: ThreadId,
    reception: GroupReceptionKeyRecord,
    entry_id: [u8; 32],
}

/// Seat one member of the community room `channel` hosts, on the shared
/// `nest` mock. `entry_seed` stands in for the roster-entry id the home nest
/// derived at seating.
fn room_seat(nest: &Arc<MockNest>, channel: ChannelId, label: &str, entry_seed: u8) -> RoomSeat {
    room_seat_as(nest, channel, label, entry_seed, ActorKeypair::generate())
}

/// [`room_seat`] under a chosen identity — for a test whose room id must
/// derive from its founder's key, so the key has to exist before the channel.
fn room_seat_as(
    nest: &Arc<MockNest>,
    channel: ChannelId,
    label: &str,
    entry_seed: u8,
    identity: ActorKeypair,
) -> RoomSeat {
    let engine = Arc::new(MlsEngine::new_in_memory(identity).unwrap());
    let actor = engine.identity_actor_id();
    let manager = ConversationsManager::new();
    let backend = Arc::new(FaunaMlsBackend::new(
        engine.clone(),
        nest.clone(),
        label,
        actor,
    ));
    manager.register_backend(backend.clone());
    // A thread to route into. The manager materializes one on first ingest,
    // which is also how a real seat's room thread comes to exist.
    manager
        .ingest_inbound(RailInboundMessage {
            rail: Rail::FaunaMls,
            sender: fauna_addr(label, actor),
            recipients: vec![],
            subject: None,
            body: "<<setup>>".into(),
            body_format: BodyFormat::PlainText,
            timestamp_ms: 1,
            message_id: MessageId(format!("setup-{label}")),
            in_reply_to: None,
            attachments: vec![],
            badges: MessageBadges::default(),
            legal_takedown_ref: None,
            plane_ref: None,
        })
        .expect("setup ingest");
    let thread = manager.snapshot().threads[0].thread_id.clone();
    backend.bind_channel(thread.clone(), channel);
    RoomSeat {
        engine,
        actor,
        backend,
        manager,
        thread,
        reception: GroupReceptionKeyRecord::mint(1_700_000_000_000),
        entry_id: [entry_seed; 32],
    }
}

impl RoomSeat {
    /// Register this seat's two community seams over one generation, wrapped
    /// to **this seat's own** roster entry — the shape
    /// `fauna.conversations.room.generations` serves ("the caller's own wraps
    /// only"). Returns the scripted reader so a test can count its calls.
    fn hold(&self, gen_key: &GenerationKey, generation_id: [u8; 32]) -> Arc<ScriptedGenerations> {
        let wrap = seal_group_generation_key_to_entry(
            gen_key,
            &self.reception.reception_pubkey().unwrap(),
            &generation_id,
            &self.entry_id,
        )
        .expect("the mint seals a wrap to this seat's reception key");
        let reader = Arc::new(ScriptedGenerations::new(vec![RoomGenerationWrap {
            generation_id,
            key_commitment: group_generation_key_commitment(gen_key),
            wrap,
            entry_id: self.entry_id,
            is_tip: true,
        }]));
        self.backend.set_room_generation_reader(reader.clone());
        self.backend
            .set_group_reception_keys(HeldReceptionKeys::holding(vec![self.reception.clone()]));
        reader
    }

    async fn poll(&self, channel: &ChannelId, after_seq: &mut i64) -> usize {
        poll_inbound_conv(&self.backend, &self.manager, channel, after_seq, 0)
            .await
            .expect("poll ok")
            .ingested
    }
}

// ── Founding: the birth ceremony, and the mint that makes it usable ────

use fauna_conversations::backend::{
    PendingRoomInvitation, RoomCeremonyRpc, RoomFloor, RoomPrincipalKind, RoomRosterRead, RoomSeams,
};
use fauna_core::group_generation::{
    GroupGenerationMintRecord, GroupMemberWrap, GroupTopupRecord, group_generation_id,
};

/// One `room.create` the ceremony seam was asked to perform.
struct CreateCall {
    salt: [u8; 32],
    policy: fauna_mls::room_policy::SignedRoomPolicy,
    reception_pubkey: Vec<u8>,
}

/// A scripted [`RoomCeremonyRpc`] standing in for the room plane's two write
/// doors, behaving as the nest does: `room.create` re-derives the room id from
/// the birth record it was handed (never echoing one the caller chose), and
/// `publish_generation` records the mint.
///
/// `answer_id` overrides the derived id, which is how a test drives the
/// founder's own re-derivation check — the one refusal that protects a thread
/// from being bound to a room its founder's key does not name.
struct ScriptedCeremony {
    creates: Mutex<Vec<CreateCall>>,
    mints: Mutex<Vec<GroupGenerationMintRecord>>,
    answer_id: Mutex<Option<String>>,
    refuse_publish: bool,
    /// Every `room.invite`, as the signed record plus the invitee node the
    /// caller named.
    invites: Mutex<Vec<(fauna_mls::room_policy::SignedRoomInvite, String)>>,
    /// Every `room.accept_invite`: `(room id hex, the wrap target handed over)`.
    accepts: Mutex<Vec<(String, Vec<u8>)>>,
    /// Every `room.backfill_generations`: `(room id hex, target actor hex,
    /// the top-up records)`.
    backfills: Mutex<Vec<(String, String, Vec<GroupTopupRecord>)>>,
    /// Every invitation this nest has **delivered** and the invitee has not yet
    /// settled: `(handle, the verbatim signed bytes)`.
    ///
    /// The nest records and delivers in one act, so an invitation staged here is
    /// exactly the set `room.invite` opened — which is what makes an invitee
    /// able to discover a room it was never told the id of.
    deliveries: Mutex<Vec<(i64, Vec<u8>)>>,
    /// The next delivery handle. An inbox row id is monotonic and never reused;
    /// nothing derives meaning from the value.
    next_delivery: Mutex<i64>,
    /// Every `room.remove`: `(room id hex, the unseated principal hex)`.
    removes: Mutex<Vec<(String, String)>>,
    /// Every `room.leave`: `(room id hex, the channel's recorded home)` — the
    /// home being what picks the relayed kind over the same-nest one.
    leaves: Mutex<Vec<(String, Option<String>)>>,
    /// When set, the nest refuses `room.leave` with this message (recording
    /// the attempt first) — the shape an end-to-end leaver meets when the home nest's
    /// door does not admit that class.
    refuse_leave: Mutex<Option<String>>,
    /// When set, the nest refuses `room.remove` with this message — the shape
    /// a caller without the rank meets.
    refuse_remove: Mutex<Option<String>>,
    /// When set, the nest LAPSES every invitation at `room.accept_invite`: it
    /// refuses with this message and consumes what it delivered — the accept
    /// door's answer to an invitation its inviter could no longer issue.
    lapse_on_accept: Mutex<Option<String>>,
    /// What this nest DOES to its floor when a removal is admitted, so a test
    /// can make the unseat a real fact the following roster read observes
    /// rather than a line in a script.
    on_remove: Mutex<Option<FloorEffect>>,
    /// Every `room.set_policy` and `room.transfer_ownership`, in order:
    /// `(door, the signed policy stored)`.
    policies: Mutex<Vec<(&'static str, fauna_mls::room_policy::SignedRoomPolicy)>>,
    /// Every `room.set_labelers`, in order — the signed set as stored.
    labeler_sets: Mutex<Vec<fauna_mls::room_policy::SignedRoomLabelers>>,
    /// Every `room.set_reception_key`: `(room id hex, the wrap target
    /// supplied)`.
    set_keys: Mutex<Vec<(String, Vec<u8>)>>,
    /// The invitations this nest holds pending on its rooms, as `room.
    /// list_invites` serves them to this caller — already scoped, as the nest
    /// scopes them. `room.revoke_invite` consumes from it.
    pending_invites: Mutex<Vec<fauna_conversations::backend::PendingRoomInvite>>,
    /// Every `room.revoke_invite`: `(room id hex, invitee hex)`.
    revokes: Mutex<Vec<(String, String)>>,
    /// When set, the nest refuses `room.list_invites` with this message — a
    /// caller off the floor, or one the door does not admit.
    refuse_list_invites: Mutex<Option<String>>,
    /// When set, the nest refuses `room.revoke_invite` with this message — a
    /// caller the scope predicate does not serve that invitation to.
    refuse_revoke: Mutex<Option<String>>,
    /// What `room.set_reception_key` answers as the tip the seat holds no wrap
    /// for — `None` is a covered seat, or a room with no generation.
    answer_uncovered_tip: Mutex<Option<[u8; 32]>>,
    /// Whether `room.set_reception_key` reports a rotation.
    answer_rotated: Mutex<bool>,
    /// What this nest DOES to its floor when a wrap target is bound —
    /// `(room id hex, the key)` — so the mint that follows reads a floor that
    /// carries it, as the real floor would.
    on_set_key: Mutex<Option<KeyEffect>>,
}

/// What a fake nest does to its own floor when it admits a membership write —
/// `(room id hex, principal hex)`.
type FloorEffect = Box<dyn Fn(&str, &str) + Send + Sync>;

/// What a fake nest does to its own floor when it binds a seat's wrap target —
/// `(room id hex, the key)`.
type KeyEffect = Box<dyn Fn(&str, &[u8]) + Send + Sync>;

impl ScriptedCeremony {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            creates: Mutex::new(Vec::new()),
            mints: Mutex::new(Vec::new()),
            answer_id: Mutex::new(None),
            refuse_publish: false,
            invites: Mutex::new(Vec::new()),
            accepts: Mutex::new(Vec::new()),
            backfills: Mutex::new(Vec::new()),
            deliveries: Mutex::new(Vec::new()),
            next_delivery: Mutex::new(1),
            removes: Mutex::new(Vec::new()),
            leaves: Mutex::new(Vec::new()),
            refuse_leave: Mutex::new(None),
            refuse_remove: Mutex::new(None),
            lapse_on_accept: Mutex::new(None),
            on_remove: Mutex::new(None),
            policies: Mutex::new(Vec::new()),
            labeler_sets: Mutex::new(Vec::new()),
            pending_invites: Mutex::new(Vec::new()),
            revokes: Mutex::new(Vec::new()),
            refuse_list_invites: Mutex::new(None),
            refuse_revoke: Mutex::new(None),
            set_keys: Mutex::new(Vec::new()),
            answer_uncovered_tip: Mutex::new(None),
            answer_rotated: Mutex::new(false),
            on_set_key: Mutex::new(None),
        })
    }
    /// A nest that answers this room id whatever birth record it is handed.
    fn answering(id_hex: &str) -> Arc<Self> {
        let c = Self::new();
        *c.answer_id.lock().unwrap() = Some(id_hex.to_string());
        c
    }
    /// A nest that founds the room but refuses to admit its mint.
    fn refusing_publish() -> Arc<Self> {
        Arc::new(Self {
            creates: Mutex::new(Vec::new()),
            mints: Mutex::new(Vec::new()),
            answer_id: Mutex::new(None),
            refuse_publish: true,
            invites: Mutex::new(Vec::new()),
            accepts: Mutex::new(Vec::new()),
            backfills: Mutex::new(Vec::new()),
            deliveries: Mutex::new(Vec::new()),
            next_delivery: Mutex::new(1),
            removes: Mutex::new(Vec::new()),
            leaves: Mutex::new(Vec::new()),
            refuse_leave: Mutex::new(None),
            refuse_remove: Mutex::new(None),
            lapse_on_accept: Mutex::new(None),
            on_remove: Mutex::new(None),
            policies: Mutex::new(Vec::new()),
            labeler_sets: Mutex::new(Vec::new()),
            pending_invites: Mutex::new(Vec::new()),
            revokes: Mutex::new(Vec::new()),
            refuse_list_invites: Mutex::new(None),
            refuse_revoke: Mutex::new(None),
            set_keys: Mutex::new(Vec::new()),
            answer_uncovered_tip: Mutex::new(None),
            answer_rotated: Mutex::new(false),
            on_set_key: Mutex::new(None),
        })
    }
    fn creates(&self) -> usize {
        self.creates.lock().unwrap().len()
    }
    /// Apply `f` to this nest's own floor whenever a removal is admitted.
    fn on_remove(&self, f: FloorEffect) {
        *self.on_remove.lock().unwrap() = Some(f);
    }
    /// The nest's own first act on both policy doors, reproduced because it is
    /// what makes the record worth signing: a policy that does not verify under
    /// the principal it names is refused before anything is stored.
    fn store_policy(&self, door: &'static str, blob: &[u8]) -> u64 {
        let signed: fauna_mls::room_policy::SignedRoomPolicy =
            fauna_core::encoding::canonical_decode(blob).expect("the policy decodes");
        signed
            .verify_signature_community()
            .expect("the author signed its own policy");
        let version = signed.policy.version;
        self.policies.lock().unwrap().push((door, signed));
        version
    }
    /// The one policy stored, and which door stored it.
    fn stored_policy(&self) -> (&'static str, fauna_mls::room_policy::SignedRoomPolicy) {
        let policies = self.policies.lock().unwrap();
        assert_eq!(policies.len(), 1, "exactly one policy was stored");
        policies[0].clone()
    }
    /// The one mint published, decomposed into the pieces a reader needs.
    fn published(&self) -> ([u8; 32], Vec<GroupMemberWrap>, [u8; 32], Vec<[u8; 32]>) {
        let mints = self.mints.lock().unwrap();
        assert_eq!(mints.len(), 1, "exactly one generation was published");
        let GroupGenerationMintRecord::Minted { core, wraps, .. } = &mints[0] else {
            panic!("a room generation is published as a Minted record");
        };
        (
            group_generation_id(core).unwrap(),
            wraps.clone(),
            core.key_commitment,
            core.parents.clone(),
        )
    }
}

#[async_trait]
impl RoomCeremonyRpc for ScriptedCeremony {
    async fn room_create(
        &self,
        salt_hex: String,
        policy: Vec<u8>,
        reception_pubkey: Vec<u8>,
    ) -> Result<String, ConvRpcError> {
        let salt: [u8; 32] = hex::decode(&salt_hex).unwrap().try_into().unwrap();
        let signed: fauna_mls::room_policy::SignedRoomPolicy =
            fauna_core::encoding::canonical_decode(&policy).expect("the policy decodes");
        signed
            .verify_signature_community()
            .expect("the founder signed its own birth policy");
        let derived = fauna_mls::room_policy::derive_room_id(&signed.policy.owner, &salt).unwrap();
        self.creates.lock().unwrap().push(CreateCall {
            salt,
            policy: signed,
            reception_pubkey,
        });
        Ok(self
            .answer_id
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| hex::encode(derived)))
    }

    async fn room_publish_generation(
        &self,
        _room_id_hex: String,
        mint: Vec<u8>,
    ) -> Result<(), ConvRpcError> {
        if self.refuse_publish {
            return Err(ConvRpcError::Rejected {
                message: "only an owner or admin mints a room generation".to_string(),
            });
        }
        self.mints
            .lock()
            .unwrap()
            .push(fauna_core::encoding::canonical_decode(&mint).expect("the mint decodes"));
        Ok(())
    }

    async fn room_invite(
        &self,
        invite: Vec<u8>,
        invitee_node: String,
        _home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        let signed: fauna_mls::room_policy::SignedRoomInvite =
            fauna_core::encoding::canonical_decode(&invite).expect("the invite decodes");
        // The nest's own first act on this door, reproduced because it is what
        // makes the record worth signing: an invitation that does not verify
        // under its named inviter is refused before anything is recorded.
        signed
            .verify_signature()
            .expect("the inviter signed its own invitation");
        let role = match signed.invite.role {
            fauna_mls::room_policy::RoomRole::Admin => "admin",
            _ => "member",
        }
        .to_string();
        // Record and DELIVER in one act, as the nest does: the invitee learns
        // the room id from the delivered record and from nowhere else.
        let mut next = self.next_delivery.lock().unwrap();
        let id = *next;
        *next += 1;
        drop(next);
        self.deliveries.lock().unwrap().push((id, invite));
        self.invites.lock().unwrap().push((signed, invitee_node));
        Ok(role)
    }

    async fn room_accept_invite(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
        _home_nest_url: Option<String>,
    ) -> Result<String, ConvRpcError> {
        if let Some(message) = self.lapse_on_accept.lock().unwrap().clone() {
            self.deliveries.lock().unwrap().retain(|(_, signed)| {
                let invite: fauna_mls::room_policy::SignedRoomInvite =
                    fauna_core::encoding::canonical_decode(signed).expect("a staged invite");
                hex::encode(&invite.invite.room_id) != room_id_hex
            });
            return Err(ConvRpcError::Rejected { message });
        }
        self.accepts
            .lock()
            .unwrap()
            .push((room_id_hex, reception_pubkey));
        Ok("member".to_string())
    }

    async fn room_backfill_generations(
        &self,
        room_id_hex: String,
        target_actor_id_hex: String,
        wraps: Vec<Vec<u8>>,
    ) -> Result<(), ConvRpcError> {
        let records = wraps
            .iter()
            .map(|b| fauna_core::encoding::canonical_decode(b).expect("the top-up decodes"))
            .collect();
        self.backfills
            .lock()
            .unwrap()
            .push((room_id_hex, target_actor_id_hex, records));
        Ok(())
    }

    async fn room_pending_invitations(&self) -> Result<Vec<PendingRoomInvitation>, ConvRpcError> {
        Ok(self
            .deliveries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, signed_invite)| PendingRoomInvitation {
                id: *id,
                signed_invite: signed_invite.clone(),
                room_node: None,
            })
            .collect())
    }

    async fn room_settle_invitation(&self, id: i64) -> Result<(), ConvRpcError> {
        self.deliveries.lock().unwrap().retain(|(d, _)| *d != id);
        Ok(())
    }

    async fn room_list_invites(
        &self,
        _room_id_hex: String,
    ) -> Result<Vec<fauna_conversations::backend::PendingRoomInvite>, ConvRpcError> {
        if let Some(message) = self.refuse_list_invites.lock().unwrap().clone() {
            return Err(ConvRpcError::Rejected { message });
        }
        Ok(self.pending_invites.lock().unwrap().clone())
    }

    async fn room_revoke_invite(
        &self,
        room_id_hex: String,
        invitee_hex: String,
    ) -> Result<bool, ConvRpcError> {
        if let Some(message) = self.refuse_revoke.lock().unwrap().clone() {
            return Err(ConvRpcError::Rejected { message });
        }
        let mut pending = self.pending_invites.lock().unwrap();
        let before = pending.len();
        pending.retain(|i| i.invitee.to_hex() != invitee_hex);
        let revoked = pending.len() != before;
        self.revokes
            .lock()
            .unwrap()
            .push((room_id_hex, invitee_hex));
        Ok(revoked)
    }

    async fn room_remove(
        &self,
        room_id_hex: String,
        principal_hex: String,
    ) -> Result<u32, ConvRpcError> {
        if let Some(message) = self.refuse_remove.lock().unwrap().clone() {
            return Err(ConvRpcError::Rejected { message });
        }
        if let Some(apply) = self.on_remove.lock().unwrap().as_ref() {
            apply(&room_id_hex, &principal_hex);
        }
        self.removes
            .lock()
            .unwrap()
            .push((room_id_hex, principal_hex));
        Ok(1)
    }

    async fn room_leave(
        &self,
        room_id_hex: String,
        home_nest_url: Option<String>,
    ) -> Result<u32, ConvRpcError> {
        self.leaves
            .lock()
            .unwrap()
            .push((room_id_hex, home_nest_url));
        if let Some(message) = self.refuse_leave.lock().unwrap().clone() {
            return Err(ConvRpcError::Rejected { message });
        }
        Ok(1)
    }

    async fn room_set_policy(
        &self,
        _room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        Ok(self.store_policy("set_policy", &policy))
    }

    async fn room_set_labelers(
        &self,
        room_id_hex: String,
        labelers: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        // The nest's own first acts on this door: the set verifies under its
        // signer, and it is bound to the room the request names.
        let signed: fauna_mls::room_policy::SignedRoomLabelers =
            fauna_core::encoding::canonical_decode(&labelers).expect("the set decodes");
        signed
            .verify_signature()
            .expect("the author signed its own labeler set");
        assert_eq!(
            hex::encode(&signed.labelers.room_id),
            room_id_hex,
            "the set names the room the request is about"
        );
        let version = signed.labelers.version;
        self.labeler_sets.lock().unwrap().push(signed);
        Ok(version)
    }

    async fn room_transfer_ownership(
        &self,
        _room_id_hex: String,
        policy: Vec<u8>,
    ) -> Result<u64, ConvRpcError> {
        Ok(self.store_policy("transfer_ownership", &policy))
    }

    async fn room_set_reception_key(
        &self,
        room_id_hex: String,
        reception_pubkey: Vec<u8>,
    ) -> Result<fauna_conversations::backend::RoomReceptionKeyBound, ConvRpcError> {
        if let Some(effect) = self.on_set_key.lock().unwrap().as_ref() {
            effect(&room_id_hex, &reception_pubkey);
        }
        self.set_keys
            .lock()
            .unwrap()
            .push((room_id_hex, reception_pubkey));
        Ok(fauna_conversations::backend::RoomReceptionKeyBound {
            entry_id: [0x5e; 32],
            rotated: *self.answer_rotated.lock().unwrap(),
            uncovered_tip: *self.answer_uncovered_tip.lock().unwrap(),
        })
    }
}

/// One floor-roster row as the home nest serves it back to a minter — the
/// **keyed** twin of [`known`], carrying the `(entry_id, reception_pubkey)`
/// pair that makes a principal a wrap target.
fn room_row(
    actor: ActorId,
    kind: RoomPrincipalKind,
    entry_seed: u8,
    reception: &GroupReceptionKeyRecord,
) -> RoomRosterKnownMember {
    RoomRosterKnownMember {
        actor,
        handle: None,
        domain: None,
        kind,
        role: Some(fauna_conversations::room::RoomRole::Owner),
        entry_id: Some([entry_seed; 32]),
        reception_pubkey: Some(reception.reception_pubkey().unwrap()),
        joined_at_ms: 1_700_000_000_000,
        tip_wrapped: None,
    }
}

/// A seat that has not yet founded anything: an engine with no group, a
/// manager with one thread to bind, and no room seams registered.
fn founder_seat(nest: &Arc<MockNest>, label: &str) -> RoomSeat {
    // `room_seat` binds its thread to a channel; a founder's channel is not
    // known until the ceremony answers, so bind it to a throwaway and let
    // `found_community_room` bind the real one. The manager keys threads, not
    // channels, so the second bind is what the thread resolves through.
    room_seat(nest, ChannelId([0xEE; 32]), label, 0xEE)
}

/// **A founded community room is keyed in the same act, and the home nest can
/// read what its founder sends** — the whole point of the class, driven
/// through the client's own doors with no nest-side helper anywhere.
///
/// The ceremony is four acts and this asserts all four
/// (`community-rooms.md` § Implementation status today, the *A community
/// room can be founded* and *The sealing lands* bullets — that build record
/// moved out of `conversation-rooms.md` on 2026-09-10): the birth record is
/// the founder's own signed act carrying its wrap target; the id is re-derived
/// rather than trusted; the room is **keyed before it is bound**, because a
/// founded room has no generation at all and the nest admits mints without
/// ever performing one; and the mint covers the home nest, which is what the
/// materialization grant *is*.
///
/// The last assertion is the load-bearing one: the message is opened with the
/// key recovered from the **nest's own wrap**, not from the founder's. That is
/// the end-to-end statement — a client-built mint grants a real read to a
/// principal the client never shared a secret with — and a mint that wrapped
/// only to its author would pass every other check here.
#[tokio::test]
async fn a_founded_room_is_keyed_and_its_home_nest_can_read_the_founders_send() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::new();
    // The home nest's own room-read keypair — minted by the nest, never named
    // by the founder.
    let nest_reception = GroupReceptionKeyRecord::mint(1_700_000_000_001);
    let nest_actor = ActorId([0x77; 32]);
    let roster = Arc::new(ScriptedReader::new(vec![Some(vec![
        room_row(
            founder.actor,
            RoomPrincipalKind::User,
            0xA1,
            &founder.reception,
        ),
        room_row(nest_actor, RoomPrincipalKind::Nest, 0xB2, &nest_reception),
    ])]));
    founder.backend.set_room_ceremony(ceremony.clone());
    founder.backend.set_room_roster_reader(roster.clone());
    founder
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![founder.reception.clone()]));

    let channel = founder
        .backend
        .found_community_room(founder.thread.clone(), Some("the commons".into()))
        .await
        .expect("the ceremony founds the room");

    // 1. The birth record is the founder's own signed act, and it carried this
    //    account's wrap target.
    let salt = {
        let creates = ceremony.creates.lock().unwrap();
        assert_eq!(creates.len(), 1, "one ceremony, not a retry storm");
        let create = &creates[0];
        assert_eq!(
            create.policy.policy.owner, founder.actor,
            "the founder is the room's one owner"
        );
        assert_eq!(create.policy.signer, founder.actor);
        assert_eq!(create.policy.policy.version, 1);
        assert_eq!(create.policy.policy.name.as_deref(), Some("the commons"));
        assert!(
            create.policy.policy.admins.is_empty(),
            "a birth record names no admins — the only user principal on the floor is the owner"
        );
        assert_eq!(
            create.reception_pubkey,
            founder.reception.reception_pubkey().unwrap(),
            "the founding roster row carries this account's wrap target"
        );
        create.salt
    };

    // 2. The channel is the id the founder's OWN key derives from the salt it
    //    chose — not merely whatever the nest answered.
    assert_eq!(
        channel.0,
        fauna_mls::room_policy::derive_room_id(&founder.actor, &salt).unwrap(),
        "the bound channel is the room the founder's key commits to"
    );

    // 3. The room was keyed in the same act, over the floor read back, with no
    //    parent — a room's first mint replaces nothing.
    let (generation_id, wraps, commitment, parents) = ceremony.published();
    assert!(parents.is_empty(), "a room's first mint names no parent");
    assert_eq!(roster.calls(), 1, "one roster read keys the room");
    assert_eq!(
        roster.homes(),
        vec![None],
        "the mint's roster read is the SAME-NEST one: a room founded here is homed here, and \
         `channel_home_url` answers `None` for the explicit same-nest marker (a relay url is \
         what `Some` means). A `Some` here would be the mint asking a foreign nest for a floor \
         it does not hold."
    );
    assert!(
        founder.backend.channel_is_same_nest(&channel),
        "and the marker is EXPLICIT rather than merely absent — recorded before the mint, so a \
         re-delivered Welcome's pre-guard cannot re-home a room this device founded"
    );
    assert_eq!(
        wraps.len(),
        2,
        "the founder and the home nest are both wrapped to"
    );

    // 4. The founder sends, and the HOME NEST opens it with its own wrap.
    founder
        .backend
        .set_room_generation_reader(Arc::new(ScriptedGenerations::new(vec![
            RoomGenerationWrap {
                generation_id,
                key_commitment: commitment,
                wrap: wraps
                    .iter()
                    .find(|w| w.entry_id == [0xA1; 32])
                    .expect("the founder's own wrap")
                    .wrap
                    .clone(),
                entry_id: [0xA1; 32],
                is_tip: true,
            },
        ])));
    founder
        .backend
        .send(
            &fauna_mls_thread(founder.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "the room is open".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("the founder sends into the room it just founded");

    let nest_wrap = wraps
        .iter()
        .find(|w| w.entry_id == [0xB2; 32])
        .expect("the home nest holds a wrap — that IS the materialization grant");
    let nest_key =
        fauna_mls::wrapped_blob::group_generation_wraps::open_group_generation_key_as_entry(
            &nest_wrap.wrap,
            &nest_reception.keypair().unwrap().secret,
            &generation_id,
            &[0xB2; 32],
            &commitment,
        )
        .expect("the nest opens the wrap the client's own mint addressed to it");

    let envelopes = nest.sent_envelopes(&channel.to_string());
    assert_eq!(envelopes.len(), 1, "one send, one envelope on the log");
    let ChannelEnvelope::RoomSealed {
        generation,
        ciphertext,
    } = ChannelEnvelope::from_bytes(&envelopes[0]).expect("the envelope decodes")
    else {
        panic!("a community room's send rides a RoomSealed envelope, never an MLS one");
    };
    assert_eq!(
        generation,
        generation_id.to_vec(),
        "the send seals under the generation the founding minted"
    );
    let opened = fauna_mls::room_message::open_room_message(
        &nest_key,
        &channel.0,
        &generation_id,
        &ciphertext,
    )
    .expect("the home nest reads the room it was granted");
    assert_eq!(opened.core.author, founder.actor);
    assert!(matches!(
        opened.core.body,
        ChannelMessageBody::Text(ref t) if t == "the room is open"
    ));
}

/// **An account with no wrap target mints one, persists it FIRST, and hands
/// out only the half it can open.**
///
/// The order is the rule (`AccountRuntimeHandle::put_group_reception_key`: "a
/// crash between posting the accept and persisting the keypair would leave a
/// delivery nothing on this account can ever open"), and this is that rule on
/// the room plane: the store refuses the write, so no ceremony must reach the
/// nest at all. A founding that pressed on would leave a real room on a real
/// nest keyed to a secret that never existed anywhere — unfixable from the
/// app, since the room is founded and its roster row is written.
#[tokio::test]
async fn a_founding_whose_reception_key_cannot_be_persisted_founds_nothing() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::new();
    founder.backend.set_room_ceremony(ceremony.clone());
    founder
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![])));
    founder
        .backend
        .set_group_reception_keys(HeldReceptionKeys::unwritable());

    let err = founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect_err("a keypair that did not reach disk founds no room");
    assert!(
        format!("{err}").contains("could not persist"),
        "the refusal names the durability failure, not a generic ceremony error: {err}"
    );
    assert_eq!(
        ceremony.creates(),
        0,
        "nothing reached the nest — the room does not exist to be repaired"
    );
}

/// **A first room mints this account's wrap target; a second reuses it.**
///
/// The scheme addresses wraps to the *account*, not to a membership
/// (`account-data-taxonomy.md` § The recipient-set scheme), so one key covers
/// every room this account sits in. A key per room would make rotation
/// O(rooms) and, worse, would leave a device that restored one room's plane
/// and not another's unable to read its own history.
#[tokio::test]
async fn the_first_room_mints_this_accounts_wrap_target_and_the_second_reuses_it() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::new();
    let keys = HeldReceptionKeys::empty();
    founder.backend.set_room_ceremony(ceremony.clone());
    founder.backend.set_group_reception_keys(keys.clone());
    // Two foundings, each keyed over a floor of one — the roster's reception
    // key is read back from what the account minted, which is exactly the
    // question here.
    let held = move || {
        let k = keys.clone();
        async move { k.reception_keys().await.into_iter().next().unwrap() }
    };
    founder
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![
            Some(vec![room_row(
                founder.actor,
                RoomPrincipalKind::User,
                0xA1,
                &GroupReceptionKeyRecord::mint(1),
            )]),
            Some(vec![room_row(
                founder.actor,
                RoomPrincipalKind::User,
                0xA1,
                &GroupReceptionKeyRecord::mint(2),
            )]),
        ])));

    founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect("the first founding mints a wrap target");
    let after_first = held().await;
    founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect("the second founding reuses it");
    let after_second = held().await;

    assert_eq!(
        after_first.reception_pubkey().unwrap(),
        after_second.reception_pubkey().unwrap(),
        "the account's wrap target is the account's, not the room's"
    );
    let creates = ceremony.creates.lock().unwrap();
    assert_eq!(creates.len(), 2);
    assert_eq!(
        creates[0].reception_pubkey, creates[1].reception_pubkey,
        "both rooms were handed the same public half"
    );
}

/// **The one room-seam registration wires the class: a backend given only the
/// [`RoomSeams`] bundle founds a community room.**
///
/// Every glue site (the wasm manager, `fauna-ffi`, linux, tui) registers the
/// four nest-backed room seams through this one call. Wired one setter at a
/// time, two glue sites once registered the roster pair and forgot the
/// ceremony and the generation read, which left web and the UniFFI apps unable
/// to found, join or open a community room. Founding needs both the ceremony
/// and the roster read, so a bundle that dropped either one would fail here.
#[tokio::test]
async fn the_room_seam_bundle_alone_wires_a_founding() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::new();
    let nest_reception = GroupReceptionKeyRecord::mint(1_700_000_000_001);
    let roster = Arc::new(ScriptedReader::new(vec![Some(vec![
        room_row(
            founder.actor,
            RoomPrincipalKind::User,
            0xA1,
            &founder.reception,
        ),
        room_row(
            ActorId([0x77; 32]),
            RoomPrincipalKind::Nest,
            0xB2,
            &nest_reception,
        ),
    ])]));
    founder.backend.set_room_seams(RoomSeams {
        reporter: Arc::new(RecordingReporter::default()),
        reader: roster.clone(),
        generations: Arc::new(ScriptedGenerations::new(vec![])),
        ceremony: ceremony.clone(),
    });
    founder
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![founder.reception.clone()]));

    founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect("the bundle's ceremony and roster read found the room");
    assert_eq!(ceremony.creates.lock().unwrap().len(), 1);
    assert_eq!(roster.calls(), 1, "the bundle's roster read keyed the room");
}

/// **A nest that answers an id the founder's own key does not derive founds
/// nothing on this device.**
///
/// The room id commits to whose key founded the room
/// (`fauna_mls::room_policy::derive_room_id`), so the answer is checkable and
/// is checked. Binding the thread to whatever came back would let a nest
/// silently point a founder at somebody else's room — one it could then read
/// every send of, since the founder would key *that* room to its own floor.
#[tokio::test]
async fn a_ceremony_answering_a_foreign_room_id_binds_nothing() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::answering(&hex::encode([0x5A; 32]));
    founder.backend.set_room_ceremony(ceremony);
    founder
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![founder.reception.clone()]));
    founder
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![])));

    let err = founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect_err("an id this founder's key does not derive is refused");
    assert!(
        format!("{err}").contains("does not derive"),
        "the refusal names the derivation mismatch: {err}"
    );
    assert!(
        !founder
            .backend
            .bound_channels()
            .contains(&ChannelId([0x5A; 32])),
        "no thread was bound to the room the nest named"
    );
}

/// **A floor that names no wrap target is refused rather than keyed to
/// nobody**, and a nest that refuses the mint leaves the room unbound.
///
/// Both are the same rule from two sides: a room is usable only once it is
/// keyed, so the founding is not "done" until the mint is admitted. A room
/// bound here would render as a thread whose every send fails with "this room
/// has no generation key on this device" — a dead thread the user cannot
/// delete their way out of and no retry repairs.
#[tokio::test]
async fn a_room_that_cannot_be_keyed_is_not_bound() {
    let nest = Arc::new(MockNest::default());

    // (a) the floor read comes back with no wrap target at all.
    let founder = founder_seat(&nest, "founder");
    founder.backend.set_room_ceremony(ScriptedCeremony::new());
    founder
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![founder.reception.clone()]));
    founder
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![Some(vec![known(
            founder.actor,
            Some("founder"),
        )])])));
    let err = founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect_err("a generation nobody can open is not a key");
    assert!(
        format!("{err}").contains("no wrap target"),
        "the refusal names the empty coverage: {err}"
    );

    // (b) the nest refuses to admit the mint.
    let second = founder_seat(&nest, "second");
    second
        .backend
        .set_room_ceremony(ScriptedCeremony::refusing_publish());
    second
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![second.reception.clone()]));
    second
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![Some(vec![room_row(
            second.actor,
            RoomPrincipalKind::User,
            0xA1,
            &second.reception,
        )])])));
    let err = second
        .backend
        .found_community_room(second.thread.clone(), None)
        .await
        .expect_err("a mint the nest refuses leaves the room unkeyed");
    assert!(
        format!("{err}").contains("owner or admin"),
        "the nest's own refusal reaches the caller: {err}"
    );
    assert_eq!(
        second.backend.bound_channels(),
        vec![ChannelId([0xEE; 32])],
        "only the fixture's own binding remains — the unkeyed room was not bound"
    );
}

/// **A device with no account plane refuses to found, by name.**
///
/// Web's declared W3 absence reaches this door: the SPA registers no
/// group-reception key seam, so it cannot mint a wrap target. Founding anyway
/// would put a real room on a real nest that nobody — including its own
/// founder — could ever key.
#[tokio::test]
async fn a_device_with_no_account_plane_refuses_to_found() {
    let nest = Arc::new(MockNest::default());
    let founder = founder_seat(&nest, "founder");
    let ceremony = ScriptedCeremony::new();
    founder.backend.set_room_ceremony(ceremony.clone());

    let err = founder
        .backend
        .found_community_room(founder.thread.clone(), None)
        .await
        .expect_err("no key seam, no room");
    assert!(
        format!("{err}").contains("group-reception key seam"),
        "the refusal names the missing seam rather than the ceremony: {err}"
    );
    assert_eq!(ceremony.creates(), 0);
}

/// **The whole join, through client doors alone: a room is founded, a second
/// seat is invited, accepts, is keyed in by its inviter, and reads what the
/// founder wrote.**
///
/// This is the class's own sequence and every step of it is a separate act on
/// purpose (`conversation-rooms.md` § Join rules and invites): **an invitation
/// does not seat anybody** — the one place the room plane diverges from the
/// group plane — and a wrap names the roster entry the *seating* derived, so
/// the three cannot be collapsed into one door however much a UI might wish
/// them to be. What this pins is that each act carries forward exactly what
/// the next one needs and nothing else.
///
/// The load-bearing assertion is the last: Bob opens Alice's message with a
/// key he recovered from **the top-up wrap Alice's device built for his own
/// roster entry** — never from anything the fixture handed him. A backfill
/// that wrapped to the wrong entry, or that this device could not have built
/// without already holding the key, fails there.
///
/// The second act carries the same burden one step earlier: Bob **discovers**
/// the room, rather than being handed its id. Nothing in this test tells him
/// what to accept into — he reads the invitation his home nest delivered, and
/// the id he accepts with is the one the record he verified names. Until that
/// delivery existed a second seat could join only if a human read the id out
/// of a log, which is why the whole class hung on this one hop.
#[tokio::test]
async fn a_second_seat_is_invited_accepts_is_keyed_in_and_reads_the_room() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let bob = founder_seat(&nest, "bob");
    let ceremony = ScriptedCeremony::new();
    // Alice founds, invites, and keys Bob in — three floor reads, and the
    // third is the one where Bob is on it.
    let alice_row = |bob_seated: bool| {
        let mut rows = vec![room_row(
            alice.actor,
            RoomPrincipalKind::User,
            0xA1,
            &alice.reception,
        )];
        if bob_seated {
            rows.push(room_row(
                bob.actor,
                RoomPrincipalKind::User,
                0xB1,
                &bob.reception,
            ));
        }
        Some(rows)
    };
    let roster = Arc::new(ScriptedReader::new(vec![
        alice_row(false), // the founding mint
        alice_row(false), // the invitation's policy-version read
        alice_row(true),  // the backfill, after Bob accepted
    ]));
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(roster.clone());
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));

    let channel = alice
        .backend
        .found_community_room(alice.thread.clone(), Some("the commons".into()))
        .await
        .expect("alice founds the room");
    let (generation_id, wraps, commitment, _) = ceremony.published();

    // Alice can read her own room from here on; the reader is what
    // `key_in_room_member` resolves the tip through.
    let alice_wrap = wraps
        .iter()
        .find(|w| w.entry_id == [0xA1; 32])
        .expect("alice's own wrap");
    alice
        .backend
        .set_room_generation_reader(Arc::new(ScriptedGenerations::new(vec![
            RoomGenerationWrap {
                generation_id,
                key_commitment: commitment,
                wrap: alice_wrap.wrap.clone(),
                entry_id: [0xA1; 32],
                is_tip: true,
            },
        ])));

    // ── 1. The invitation is alice's own signed act, and seats nobody. ──
    alice
        .backend
        .invite_to_room(
            &channel,
            bob.actor,
            fauna_mls::room_policy::RoomRole::Member,
            None,
        )
        .await
        .expect("the owner invites under the default `invite` join rule");
    {
        let invites = ceremony.invites.lock().unwrap();
        assert_eq!(invites.len(), 1);
        let (signed, node) = &invites[0];
        assert_eq!(signed.inviter, alice.actor, "the inviter signed it herself");
        assert_eq!(signed.invite.invitee, bob.actor);
        assert_eq!(signed.invite.role, fauna_mls::room_policy::RoomRole::Member);
        assert_eq!(
            signed.invite.room_id,
            channel.0.to_vec(),
            "the invitation names the room, so it survives the nest boundary intact"
        );
        assert_eq!(
            signed.invite.policy_version, 1,
            "the version alice READ the join rule under — from the floor read that served the \
             roles, never a cache that could name a policy under which her own rank differed"
        );
        assert!(node.is_empty(), "a same-nest invitee names no node");
        assert!(
            ceremony.backfills.lock().unwrap().is_empty(),
            "an invitation is not a seating: nothing is keyed until bob accepts, because a wrap \
             names the roster entry the SEATING derives"
        );
    }

    // ── 2. Bob DISCOVERS the invitation, then accepts it. ──
    //
    // The discovery is the load-bearing half: nothing in this test hands bob
    // the room id. He learns it from the invitation his home nest delivered
    // him, which is the only way a second seat ever reaches a room
    // (`conversation-rooms.md` § Join rules and invites — an invite is
    // "delivered to the invitee's home nest through the inbox plane").
    bob.backend.set_room_ceremony(ceremony.clone());
    bob.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![bob.reception.clone()]));
    let pending = bob
        .backend
        .pending_room_invitations()
        .await
        .expect("bob reads his own standing invitations");
    assert_eq!(pending.len(), 1, "the one invitation alice opened");
    let invitation = &pending[0];
    assert_eq!(
        invitation.room_id, channel.0,
        "the delivered record names the room — this is the id bob accepts with, and he was told \
         it by nothing else"
    );
    assert_eq!(
        invitation.inviter, alice.actor,
        "verified against the signature alice wrote, not asserted by the delivery path"
    );
    assert_eq!(invitation.role, fauna_conversations::room::RoomRole::Member);
    assert_eq!(
        invitation.policy_version, 1,
        "the version alice read the join rule under travels with the invitation"
    );
    assert!(
        invitation.room_node.is_none(),
        "a room homed on the invitee's own nest names no next hop"
    );

    let bob_channel = bob
        .backend
        .accept_room_invite(
            bob.thread.clone(),
            invitation.room_id,
            invitation.room_node.clone(),
        )
        .await
        .expect("bob accepts his own pending invitation");
    assert_eq!(bob_channel, channel, "both seats are on one room");
    {
        let accepts = ceremony.accepts.lock().unwrap();
        assert_eq!(accepts.len(), 1);
        assert_eq!(accepts[0].0, channel.to_string());
        assert_eq!(
            accepts[0].1,
            bob.reception.reception_pubkey().unwrap(),
            "the roster row and the wrap target are one fact — a member cannot be seated without \
             the room knowing how to key it"
        );
    }
    assert!(
        bob.backend.channel_is_same_nest(&channel),
        "the joiner marks the room's home explicitly, like the founder"
    );
    // Settling is what consumes a delivered invitation — an accepted one must
    // stop standing, or bob's app would offer him a room he is already seated
    // in for as long as the record survived.
    bob.backend
        .settle_room_invitation(invitation.id)
        .await
        .expect("bob settles the invitation he accepted");
    assert!(
        bob.backend
            .pending_room_invitations()
            .await
            .expect("the read still works")
            .is_empty(),
        "an accepted invitation no longer stands"
    );

    // ── 3. Alice keys bob in. An ADD never mints. ──
    alice
        .backend
        .key_in_room_member(&channel, bob.actor)
        .await
        .expect("the inviter covers the newcomer she invited");
    assert_eq!(
        ceremony.mints.lock().unwrap().len(),
        1,
        "still exactly ONE mint — the founding's. An add wraps what exists to the new entry and \
         never rotates (`account-data-taxonomy.md` § The recipient-set scheme → *Mint triggers*)"
    );
    let bob_wrap = {
        let backfills = ceremony.backfills.lock().unwrap();
        assert_eq!(backfills.len(), 1);
        let (room, target, records) = &backfills[0];
        assert_eq!(room, &channel.to_string());
        assert_eq!(target, &hex::encode(bob.actor.0));
        assert_eq!(
            records.len(),
            1,
            "the TIP alone: no client can read a room's history policy, and a batch naming one \
             unauthorized generation is refused WHOLE — which would leave the newcomer with \
             nothing rather than with less"
        );
        let GroupTopupRecord::Wrap {
            generation_id: g,
            target_entry,
            healer,
            wrap,
            ..
        } = &records[0];
        assert_eq!(*g, generation_id, "the room's tip, not a fresh key");
        assert_eq!(
            *target_entry, [0xB1; 32],
            "bound to the entry bob's SEATING derived — a re-admitted member returns on a fresh \
             entry, and a wrap to a retired one would undo exactly what that buys"
        );
        assert_eq!(
            *healer, alice.actor.0,
            "the healer is the actor the nest binds to the authenticated caller"
        );
        wrap.clone()
    };

    // ── 4. Bob reads the room, with the key he recovered from that wrap. ──
    bob.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![bob.reception.clone()]));
    bob.backend
        .set_room_generation_reader(Arc::new(ScriptedGenerations::new(vec![
            RoomGenerationWrap {
                generation_id,
                key_commitment: commitment,
                wrap: bob_wrap,
                entry_id: [0xB1; 32],
                is_tip: true,
            },
        ])));
    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "welcome to the commons".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice sends");

    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let detail = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread");
    let bubble = detail
        .messages
        .iter()
        .find(|m| m.body == "welcome to the commons")
        .expect("the newcomer reads the room he was keyed into");
    assert_eq!(
        bubble.sender.person_actor_id(),
        Some(alice.actor),
        "and the bubble names its author — the signed claim, not the key, is what attributes it"
    );
}

/// A roster reader over a **live** floor the ceremony mutates, rather than a
/// scripted sequence of answers.
///
/// The severance tests turn on an *ordering* — the unseat must land before the
/// mint reads the floor — and a scripted reader cannot witness one: it would
/// answer whatever the script said next whichever order the calls came in, so a
/// swapped implementation would still pass. Here the fake nest's `room.remove`
/// removes the row, and the mint sees the floor as it actually stands.
struct LiveFloor {
    members: Mutex<Vec<RoomRosterKnownMember>>,
    policy: Mutex<Option<Vec<u8>>>,
    labelers: Mutex<Option<Vec<u8>>>,
    /// The retained versions a versioned read serves, version 1 first, and the
    /// room's birth salt beside them — empty for a floor that retains none.
    versions: Mutex<Vec<fauna_mls::room_policy::SignedRoomPolicy>>,
    salt: Mutex<Option<[u8; 32]>>,
}

impl LiveFloor {
    fn holding(members: Vec<RoomRosterKnownMember>) -> Arc<Self> {
        Arc::new(Self {
            members: Mutex::new(members),
            policy: Mutex::new(None),
            labelers: Mutex::new(None),
            versions: Mutex::new(Vec::new()),
            salt: Mutex::new(None),
        })
    }
    /// Serve this signed policy back on every read.
    fn holding_policy(&self, signed: &fauna_mls::room_policy::SignedRoomPolicy) {
        *self.policy.lock().unwrap() =
            Some(fauna_core::encoding::canonical_encode(signed).unwrap());
    }
    /// Found a community room for `seat` whose current policy is `current`,
    /// and serve its whole chain: a birth record naming `current`'s name, then
    /// each version up to `current`'s, all signed by `seat` for this room. The
    /// room id derives from `seat`'s key, as a founded room's does — so the
    /// chain anchors, and a device amends only a policy it has proven to be
    /// this room's. Answers the room.
    fn holding_chain_to(
        &self,
        seat: &RoomSeat,
        current: &fauna_mls::room_policy::RoomPolicy,
    ) -> ChannelId {
        use fauna_mls::room_policy::{RoomPolicy, binding_birth_salt, derive_room_id};
        let salt = binding_birth_salt(&[0x5A; 24]);
        let channel = ChannelId(derive_room_id(&seat.actor, &salt).unwrap());
        let sign = |policy: &RoomPolicy| {
            seat.engine
                .sign_room_policy_community(&channel.0, policy)
                .expect("signs")
        };
        let birth = RoomPolicy::initial(seat.actor, current.name.clone());
        let mut versions = vec![sign(&birth)];
        for version in 2..current.version {
            versions.push(sign(&RoomPolicy {
                version,
                ..birth.clone()
            }));
        }
        if current.version > 1 {
            versions.push(sign(current));
        }
        self.holding_policy(versions.last().unwrap());
        *self.versions.lock().unwrap() = versions;
        *self.salt.lock().unwrap() = Some(salt);
        seat.backend.bind_channel(seat.thread.clone(), channel);
        channel
    }
    /// Retain `signed` as the room's next version and serve it as current —
    /// what the home nest does once it admits a change.
    fn appending(&self, signed: &fauna_mls::room_policy::SignedRoomPolicy) {
        self.versions.lock().unwrap().push(signed.clone());
        self.holding_policy(signed);
    }
    /// Serve these labeler-set bytes back on every read — whatever they are,
    /// since a floor read is a transport and the reader is what verifies.
    fn holding_labelers(&self, bytes: Vec<u8>) {
        *self.labelers.lock().unwrap() = Some(bytes);
    }
    fn unseat(&self, who: ActorId) {
        self.members.lock().unwrap().retain(|m| m.actor != who);
    }
    /// Bind `key` as `who`'s wrap target — what the home nest does to its
    /// floor when `room.set_reception_key` is admitted.
    fn rekey(&self, who: ActorId, key: &[u8]) {
        for m in self.members.lock().unwrap().iter_mut() {
            if m.actor == who {
                m.reception_pubkey = Some(key.to_vec());
            }
        }
    }
    fn seats(&self) -> Vec<ActorId> {
        self.members
            .lock()
            .unwrap()
            .iter()
            .map(|m| m.actor)
            .collect()
    }
}

#[async_trait]
impl RoomRosterReader for LiveFloor {
    async fn read_roster(
        &self,
        _channel_hex: String,
        _home_nest_url: Option<String>,
    ) -> RoomRosterRead {
        RoomRosterRead::Floor(RoomFloor {
            members: self.members.lock().unwrap().clone(),
            policy_version: Some(1),
            policy: self.policy.lock().unwrap().clone(),
            labelers: self.labelers.lock().unwrap().clone(),
        })
    }

    async fn read_policy_version(
        &self,
        _channel: String,
        _home: Option<String>,
        version: u64,
    ) -> fauna_conversations::backend::RoomPolicyVersionRead {
        use fauna_conversations::backend::RoomPolicyVersionRead;
        match self
            .versions
            .lock()
            .unwrap()
            .get((version as usize).wrapping_sub(1))
        {
            Some(signed) => RoomPolicyVersionRead::Served {
                policy: fauna_core::encoding::canonical_encode(signed).unwrap(),
                birth_salt: *self.salt.lock().unwrap(),
            },
            None => RoomPolicyVersionRead::NotHeld,
        }
    }
}

/// A generation reader that answers one tip whose wrap this device cannot
/// open — the honest shape for a **rotation**, which needs the tip's name and
/// not its key.
struct TipOnly {
    generation_id: [u8; 32],
}

#[async_trait]
impl RoomGenerationReader for TipOnly {
    async fn read_generations(
        &self,
        _room_id_hex: String,
        _home_nest_url: Option<String>,
    ) -> Option<Vec<RoomGenerationWrap>> {
        Some(vec![RoomGenerationWrap {
            generation_id: self.generation_id,
            key_commitment: [0x11; 32],
            wrap: vec![0xde, 0xad],
            entry_id: [0xA1; 32],
            is_tip: true,
        }])
    }
}

/// **A removal unseats AND rotates, in that order** — the severance the class's
/// key model rests on (`conversation-rooms.md` § The three classes →
/// *Community*, reason 4: "a removal rotates the generation, so a removed
/// member who can still fetch ciphertext through a relay reads nothing new").
///
/// Unseating alone severs nothing. A removed member keeps every generation key
/// it was ever wrapped into, and a room's ciphertext is fetchable by anyone the
/// relay serves — so a `room.remove` door without a rotation behind it would
/// leave every caller believing in a severance that had not happened. The nest
/// never mints, so this is the client's job and nobody else's.
///
/// The **order** is what this pins, and it is why the floor here is live rather
/// than scripted: coverage is judged against the floor as it stands *now*, so a
/// mint built before the unseat would wrap the room's next key to the very
/// member being removed. The assertion that catches a swap is the last one —
/// bob's entry is absent from the new wrap set — and it is red-verified by
/// rotating first.
#[tokio::test]
async fn a_removal_unseats_and_rotates_the_room_key_without_the_removed_member() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let bob = ActorId([0xB0; 32]);
    let bob_reception = GroupReceptionKeyRecord::mint(1_700_000_000_000);
    let ceremony = ScriptedCeremony::new();

    let floor = LiveFloor::holding(vec![
        room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception),
        room_row(bob, RoomPrincipalKind::User, 0xB1, &bob_reception),
    ]);
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(floor.clone());
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));

    // A room already keyed: the tip this rotation must name as its parent.
    let founding_generation = [0x6F; 32];
    alice.backend.set_room_generation_reader(Arc::new(TipOnly {
        generation_id: founding_generation,
    }));
    let channel = ChannelId([0x5A; 32]);

    // The fake nest applies the unseat to the floor the mint will read — which
    // is the whole point: the order is a fact of the run, not of a script.
    {
        let floor = floor.clone();
        ceremony.on_remove(Box::new(move |_room, principal| {
            floor.unseat(ActorId(
                <[u8; 32]>::try_from(hex::decode(principal).unwrap().as_slice()).unwrap(),
            ));
        }));
    }

    alice
        .backend
        .remove_room_member(&channel, bob)
        .await
        .expect("an owner removes a member and severs them in one act");

    {
        let removes = ceremony.removes.lock().unwrap();
        assert_eq!(removes.len(), 1, "one unseat");
        assert_eq!(removes[0].0, channel.to_string());
        assert_eq!(removes[0].1, hex::encode(bob.0));
    }
    assert_eq!(
        floor.seats(),
        vec![alice.actor],
        "bob is off the floor before the mint reads it"
    );

    let mints = ceremony.mints.lock().unwrap();
    assert_eq!(mints.len(), 1, "the removal rotated the room's key");
    let GroupGenerationMintRecord::Minted { core, wraps, .. } = &mints[0] else {
        panic!("a room generation is published as a Minted record");
    };
    assert_eq!(
        core.parents,
        vec![founding_generation],
        "the rotation builds on the room's current tip — which is also the check the nest \
         admits it on"
    );
    let entries: Vec<[u8; 32]> = wraps.iter().map(|w| w.entry_id).collect();
    assert!(
        entries.contains(&[0xA1; 32]),
        "the members who remain are keyed into the new generation"
    );
    assert!(
        !entries.contains(&[0xB1; 32]),
        "and the removed member is NOT — a rotation that still wrapped to them would hand out \
         exactly what it exists to withhold"
    );
}

/// **A rotation that does not land is named, not swallowed** — and what is left
/// behind is recoverable.
///
/// By the time a rotation can fail the unseat has landed, so the honest report
/// is neither "removed" nor "failed": the target is off the floor and cannot
/// send, but can still open traffic sealed under the un-rotated tip. A caller
/// told "the removal failed" would go looking for the wrong problem — and, far
/// worse, a caller told nothing at all would believe in a severance that had
/// not happened.
///
/// The residue is deliberately not rolled back by re-seating the target:
/// re-admission returns a member on a **fresh** roster entry, so an undo would
/// not restore the state that was there. `rotate_room_key` is the retry
/// instead, runnable by any owner or admin from any device.
#[tokio::test]
async fn a_removal_whose_rotation_fails_names_the_unsevered_residue() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let bob = ActorId([0xB0; 32]);
    let ceremony = ScriptedCeremony::new();
    let floor = LiveFloor::holding(vec![room_row(
        alice.actor,
        RoomPrincipalKind::User,
        0xA1,
        &alice.reception,
    )]);
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(floor);
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));
    // No generation reader at all: this device cannot see the room's tip, so it
    // cannot name the parent a mint must build on.
    let channel = ChannelId([0x5A; 32]);

    let err = alice
        .backend
        .remove_room_member(&channel, bob)
        .await
        .expect_err("a removal whose rotation cannot be built is not a success");

    let err = format!("{err}");
    assert!(
        err.contains(&hex::encode(bob.0)),
        "the report names WHO is unsevered: {err}"
    );
    assert!(
        err.contains("NOT rotated"),
        "and says the key was not rotated, rather than reading as a failed removal: {err}"
    );
    assert!(
        err.contains("rotate again"),
        "and names the retry that completes it: {err}"
    );
    assert_eq!(
        ceremony.removes.lock().unwrap().len(),
        1,
        "the unseat DID land — which is exactly why the residue has to be named"
    );
    assert!(
        ceremony.mints.lock().unwrap().is_empty(),
        "and nothing was minted"
    );
}

/// **Leaving unseats and does NOT rotate** — an absence with a reason, not an
/// omission.
///
/// A leaver has no mint authority (key authority is the owner's and admins',
/// § Roles and authorization), and a rotation it could build would still wrap
/// to itself: coverage is judged against the floor, and the leaver is on it
/// until the unseat lands. What bounds a voluntary departure is that the leaver
/// already read everything up to now — a room that wants a departed member
/// sealed out of new traffic rotates from a remaining owner or admin.
///
/// The device's own state is left alone for the same reason: deleting the
/// generations and bubbles it already holds would destroy the user's own copy
/// of a conversation they were legitimately part of, and would not un-read a
/// single byte.
#[tokio::test]
async fn leaving_a_room_unseats_this_account_and_mints_nothing() {
    let nest = Arc::new(MockNest::default());
    let seat = founder_seat(&nest, "leaver");
    let ceremony = ScriptedCeremony::new();
    seat.backend.set_room_ceremony(ceremony.clone());
    seat.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![seat.reception.clone()]));
    let channel = ChannelId([0x5A; 32]);

    seat.backend
        .leave_room_by_ceremony(&channel)
        .await
        .expect("a member leaves");

    assert_eq!(
        ceremony.leaves.lock().unwrap().as_slice(),
        &[(channel.to_string(), None)],
        "the departure names its room and nothing else — the caller IS the principal — \
         and a room with no recorded foreign home leaves by the same-nest door"
    );
    assert!(
        ceremony.mints.lock().unwrap().is_empty(),
        "a leaver mints nothing: it has no key authority, and a mint it could build would wrap \
         to itself anyway"
    );
    assert!(
        ceremony.removes.lock().unwrap().is_empty(),
        "leaving is its own door — never a self-addressed remove"
    );
}

/// **A foreign-homed room's departure carries the room's home**
/// (`conversation-rooms.md` § The home nest — "a member on a foreign nest
/// reaches the room only through their own home nest, which originates the leg
/// to the room's home"), by the same `ChannelHome` signal that picks `send` vs
/// `send_remote` and routes a room's attachment bytes.
///
/// Discriminating by construction: before this the seam had no home parameter
/// at all, so every departure went to the leaver's own nest — which holds no
/// room record for a room homed elsewhere and answers "no such room", leaving
/// the member seated on the only floor that counts. The seat is not inert
/// there: the roster-coverage gate obliges every later generation mint to wrap
/// to its reception key, and `room.invite` refuses to re-admit a principal that
/// is already a member, so the ghost keeps drawing key material AND blocks the
/// re-admission that would heal it.
#[tokio::test]
async fn leaving_a_foreign_homed_room_departs_through_the_rooms_home() {
    let nest = Arc::new(MockNest::default());
    let seat = founder_seat(&nest, "foreign-leaver");
    let ceremony = ScriptedCeremony::new();
    seat.backend.set_room_ceremony(ceremony.clone());
    let channel = ChannelId([0x5B; 32]);
    seat.backend
        .record_channel_home(channel, "https://home.example");

    seat.backend
        .leave_room_by_ceremony(&channel)
        .await
        .expect("a foreign-homed member leaves");

    assert_eq!(
        ceremony.leaves.lock().unwrap().as_slice(),
        &[(
            channel.to_string(),
            Some("https://home.example".to_string())
        )],
        "the departure names the room's HOME, which is what routes it onto the relay kind"
    );
    assert!(
        ceremony.mints.lock().unwrap().is_empty(),
        "a relayed leaver mints nothing either — the no-rotation rule is the room's, not the \
         homing's"
    );
}

/// **A member of an end-to-end room leaves by the self-scoped leave door** —
/// the same `room.leave` a community room's member opens
/// (`conversation-rooms.md` § Roles and authorization → *Leaving — the
/// mechanism*, amended 2026-09-23).
///
/// Three things are asserted together because each is a different way the
/// gesture could be built wrong:
///
///   * the door is asked, naming this room — and **no roster report** is
///     sent. The report was this class's departure until the amendment: built
///     from this device's own group view at no position, it replaced the floor
///     wholesale, so a leaver behind the newest membership commit rolled the
///     floor back. The door stamps this account's row and no one
///     else's, whatever this device's view;
///   * **no envelope is posted**. MLS never lets a commit remove its own
///     committer, so there is no self-unseating commit to make;
///   * **nothing is minted** — a leaver holds no key authority.
#[tokio::test]
async fn a_member_leaves_an_end_to_end_room_by_the_self_scoped_door() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    let detail = bob.manager.thread_detail(bob.tid.clone()).unwrap();
    assert_eq!(
        detail.room.as_ref().unwrap().my_role,
        Some(RoomRole::Member)
    );
    assert!(
        detail.capabilities.can_leave_room,
        "an ordinary member may walk out — the one room verb that is not the \
         owner's or an admin's"
    );

    let ceremony = ScriptedCeremony::new();
    bob.backend.set_room_ceremony(ceremony.clone());
    let reporter = Arc::new(RecordingReporter::default());
    bob.backend.set_room_roster_reporter(reporter.clone());
    let before = g.nest.sent_envelopes(&g.channel_id.to_string()).len();

    bob.manager.leave_room(bob.tid.clone()).await;
    assert!(
        bob.manager.snapshot().error.is_none(),
        "a member's departure is not refused"
    );

    assert_eq!(
        ceremony.leaves.lock().unwrap().as_slice(),
        &[(g.channel_id.to_string(), None)],
        "one departure, through the leave door, naming this room"
    );
    assert!(
        reporter.reports.lock().unwrap().is_empty(),
        "and no roster report: a departure names no one but the leaver"
    );
    assert_eq!(
        g.nest.sent_envelopes(&g.channel_id.to_string()).len(),
        before,
        "leaving commits nothing: MLS never lets a commit remove its own committer"
    );
    assert!(
        ceremony.mints.lock().unwrap().is_empty(),
        "a leaver holds no key authority, so it mints nothing"
    );
}

/// **The thread stays on the leaver's own device.** Deleting the generations
/// and bubbles it already holds would destroy the user's own copy of a
/// conversation they were legitimately part of, and would not un-read a byte
/// ([`community-rooms.md`](community-rooms.md) § Implementation status today,
/// the leave paragraph — the same posture the community class ratified).
#[tokio::test]
async fn leaving_keeps_the_leavers_own_copy_of_the_conversation() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    bob.backend.set_room_ceremony(ScriptedCeremony::new());

    bob.manager.leave_room(bob.tid.clone()).await;

    assert!(
        bob.manager.thread_detail(bob.tid.clone()).is_some(),
        "the conversation is still the user's to read"
    );
    assert!(
        bob.manager
            .snapshot()
            .threads
            .iter()
            .any(|t| t.thread_id == bob.tid),
        "and it is still listed — a leaver keeps its own copy"
    );
}

/// **The owner transfers first.** Both nest doors enforce "a room is never
/// owner-less" on their own — one by rank, the other by refusing a governed
/// roster that names no owner — so this refusal buys the user a sentence
/// naming the remedy rather than a wire refusal about a malformed room.
#[tokio::test]
async fn the_owner_cannot_leave_until_the_room_is_handed_over() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let detail = alice.manager.thread_detail(alice.tid.clone()).unwrap();
    assert_eq!(detail.room.as_ref().unwrap().my_role, Some(RoomRole::Owner));
    assert!(
        !detail.capabilities.can_leave_room,
        "the roles table greys the owner's own leave"
    );

    let reporter = Arc::new(RecordingReporter::default());
    alice.backend.set_room_roster_reporter(reporter.clone());
    alice.manager.leave_room(alice.tid.clone()).await;

    let (key, message) = page_error_message(&alice.manager);
    assert_eq!(key, "conversations.unified.error_leave_room");
    assert_eq!(message, send_errors::ROOM_OWNER_CANNOT_LEAVE);
    assert!(
        reporter.reports.lock().unwrap().is_empty(),
        "refused before the wire: an owner-less roster never leaves this device"
    );
}

/// **A refused door is surfaced, never swallowed.** The leave door refused,
/// so the honest answer is that the user is still in the room — the door's
/// own refusal text, verbatim, since [`BackendError::Refusal`]'s payload IS
/// the user-facing sentence.
#[tokio::test]
async fn a_refused_door_is_surfaced_to_the_leaver() {
    let g = governed_room();
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    let ceremony = ScriptedCeremony::new();
    *ceremony.refuse_leave.lock().unwrap() = Some("not a member of this room".to_string());
    bob.backend.set_room_ceremony(ceremony);

    bob.manager.leave_room(bob.tid.clone()).await;

    let (key, message) = page_error_message(&bob.manager);
    assert_eq!(key, "conversations.unified.error_leave_room");
    assert_eq!(message, "not a member of this room");
}

/// **DECLARED RESIDUE — a remaining member's next commit re-seats the
/// leaver, until the leaf actually goes** (`conversation-rooms.md`
/// § Implementation status today, the departed-leaf gap).
///
/// This is the cost of the ruling that a self-leave is a floor act: the
/// leaver's MLS leaf stays until a remaining owner or admin commits the
/// Remove, so that member's engine roster still names the leaver, and the
/// report its next membership commit owes re-seats them on the floor.
///
/// Pinned rather than left to be discovered: the test is green **because the
/// hole is real**, and it turns red the day a remaining device learns to
/// reconcile a departed floor row — which is exactly when someone should be
/// reading this.
#[tokio::test]
async fn a_remaining_members_next_commit_re_seats_the_leaver_until_the_leaf_goes() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let bob = seat(
        g.bob.clone(),
        &g.nest,
        "bob",
        g.channel_id,
        vec![
            fauna_addr("alice", g.alice.identity_actor_id()),
            fauna_addr("carol", g.carol.identity_actor_id()),
        ],
    );
    bob.backend.set_room_ceremony(ScriptedCeremony::new());
    bob.manager.leave_room(bob.tid.clone()).await;
    assert!(bob.manager.snapshot().error.is_none(), "bob has left");

    // Alice now makes an unrelated membership/policy commit. Her engine never
    // heard about the departure — nothing unseated bob's leaf — so the roster
    // she owes a report for still names him.
    let reporter = Arc::new(RecordingReporter::default());
    alice.backend.set_room_roster_reporter(reporter.clone());
    alice
        .manager
        .appoint_admin(
            alice.tid.clone(),
            fauna_addr("carol", g.carol.identity_actor_id()),
        )
        .await;
    assert!(
        alice.manager.snapshot().error.is_none(),
        "the owner appoints"
    );

    let reports = reporter.reports.lock().unwrap();
    let report = reports.last().expect("the commit owes a report");
    let reported: Vec<ActorId> = report.members.iter().map(|m| m.actor).collect();
    assert!(
        reported.contains(&bob.actor),
        "DECLARED RESIDUE: bob is back on the reported roster, because his leaf          is still in the group. Closing this means a remaining owner or admin          reconciling a departed floor row by committing the Remove — when that          lands, this assertion is what tells you to delete the residue note in          `conversation-rooms.md` § Implementation status today."
    );
}

/// **A seamless app refuses these doors by name rather than half-acting.**
///
/// The refusal matters most for the removal: a device that unseated a member
/// and then found it had no way to mint would be the residue case, reached by
/// an app that never could have completed the act. Naming it up front is the
/// same posture founding takes for a device with no account plane.
#[tokio::test]
async fn the_membership_doors_refuse_without_a_ceremony_seam() {
    let nest = Arc::new(MockNest::default());
    let seat = founder_seat(&nest, "seamless");
    let channel = ChannelId([0x5A; 32]);

    let err = format!(
        "{}",
        seat.backend
            .remove_room_member(&channel, ActorId([0xB0; 32]))
            .await
            .expect_err("no seam, no removal")
    );
    assert!(err.contains("room-ceremony seam"), "{err}");
    let err = format!(
        "{}",
        seat.backend
            .leave_room_by_ceremony(&channel)
            .await
            .expect_err("no seam, no departure")
    );
    assert!(err.contains("room-ceremony seam"), "{err}");
}

/// A seat holding a room whose policy the fake nest serves back — the base
/// every policy-door test amends from.
fn policy_seat(
    nest: &Arc<MockNest>,
    label: &str,
) -> (RoomSeat, Arc<ScriptedCeremony>, Arc<LiveFloor>) {
    let seat = founder_seat(nest, label);
    let ceremony = ScriptedCeremony::new();
    let floor = LiveFloor::holding(vec![room_row(
        seat.actor,
        RoomPrincipalKind::User,
        0xA1,
        &seat.reception,
    )]);
    seat.backend.set_room_ceremony(ceremony.clone());
    seat.backend.set_room_roster_reader(floor.clone());
    (seat, ceremony, floor)
}

/// **A policy change amends the STORED bytes — every field the edit does not
/// name survives** (`conversation-rooms.md` § Roles and authorization).
///
/// A change is a *replacement* at `version + 1`, not a patch: the whole
/// document is re-signed and stored. So the failure this guards against is not
/// a rejected edit but an accepted one that quietly reset a field the editor
/// never showed the user — a rename that also turned history off, because the
/// app rebuilt the policy from what it happened to know. That is exactly what
/// an app was forced to do before the roster read served the policy bytes, and
/// it is why the read is the prerequisite these doors waited on.
///
/// The version bump is the nest's own ratchet, so two devices editing at once
/// means the second is refused rather than silently winning.
#[tokio::test]
async fn a_policy_edit_changes_only_what_it_names_and_bumps_the_version_by_one() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");

    let mut stored =
        fauna_mls::room_policy::RoomPolicy::initial(seat.actor, Some("the commons".into()));
    stored.version = 4;
    stored.history_policy = fauna_mls::room_policy::HistoryPolicy::Full;
    stored.set_admins(vec![ActorId([0xAD; 32])]);
    let channel = floor.holding_chain_to(&seat, &stored);

    let version = seat
        .backend
        .set_room_policy(
            &channel,
            RoomPolicyEdit::JoinRule(fauna_conversations::room::JoinRule::Request),
        )
        .await
        .expect("the owner opens the room to requests");

    assert_eq!(version, 5, "exactly one past the stored version");
    let (door, signed) = ceremony.stored_policy();
    assert_eq!(door, "set_policy");
    assert_eq!(signed.policy.version, 5);
    assert_eq!(
        signed.policy.join_rule,
        fauna_mls::room_policy::JoinRule::Request,
        "the field the edit named changed — and `request` is a COMMUNITY room's rule, which \
         only the community signer will put a signature on"
    );
    assert_eq!(
        signed.policy.name.as_deref(),
        Some("the commons"),
        "and the name it did not name survived — an edit is not a rebuild"
    );
    assert_eq!(
        signed.policy.history_policy,
        fauna_mls::room_policy::HistoryPolicy::Full,
        "so did the history policy"
    );
    assert_eq!(
        signed.policy.admins,
        vec![ActorId([0xAD; 32])],
        "and so did the admin set — which only the owner may change at all"
    );
    assert_eq!(
        signed.policy.owner, seat.actor,
        "ownership is not settable here; it moves through its own door"
    );
}

/// Seat the home nest on a policy seat's floor — what makes the room a
/// **community** room, the one class whose home nest reads and so can label.
fn seat_home_nest(floor: &LiveFloor) {
    floor.members.lock().unwrap().push(room_row(
        ActorId([0x0E; 32]),
        RoomPrincipalKind::Nest,
        0x0E,
        &GroupReceptionKeyRecord::mint(1_700_000_000_000),
    ));
}

/// **What reads a community room reaches its snapshot — verified, or not at
/// all** (`conversation-rooms.md` § The three classes → *What the home nest
/// does with its read*, purpose 2, and rule 6: members verify the set).
///
/// The floor read is a transport, so the set it serves is a claim until the
/// reader checks it: its signature, and that it names THIS room — a set signed
/// for another room the same admin holds must not render here. A community
/// room naming none is an empty set, a real answer the editor stages from; a
/// set that does not verify is no answer at all, which the editor does not
/// paint rather than paint as a guess.
#[tokio::test]
async fn a_community_rooms_verified_labeler_set_reaches_its_snapshot() {
    /// A fresh seat whose floor serves `bytes` as the room's labeler set (and
    /// seats the home nest when `community`): what its snapshot says reads the
    /// room once the poll pass has read the floor. Fresh per case, because a
    /// group-less floor is read once per session here and re-read on the tend
    /// cadence, not on every call.
    async fn seen(bytes: Option<Vec<u8>>, community: bool) -> Option<Vec<String>> {
        let nest = Arc::new(MockNest::default());
        let (seat, _ceremony, floor) = policy_seat(&nest, "owner");
        let channel = ChannelId([0x5A; 32]);
        seat.backend.bind_channel(seat.thread.clone(), channel);
        if community {
            seat_home_nest(&floor);
        }
        if let Some(bytes) = bytes {
            floor.holding_labelers(bytes);
        }
        seat.backend
            .resolve_nameless_members(&seat.manager, &channel)
            .await;
        let mut detail = seat
            .manager
            .thread_detail(seat.thread.clone())
            .expect("thread");
        detail.participants = vec![fauna_conversations::address::TypedAddress::Fauna {
            actor_id: seat.actor,
            handle: String::new(),
        }];
        RailBackend::room_state(seat.backend.as_ref(), &detail)
            .expect("a snapshot")
            .labelers
    }
    let channel = [0x5Au8; 32];
    let labeler = ActorId([0x1B; 32]);
    let admin = fauna_core::identity::ActorKeypair::from_secret([0x0A; 32]);
    let encode = |signed: &fauna_mls::room_policy::SignedRoomLabelers| {
        Some(fauna_core::encoding::canonical_encode(signed).unwrap())
    };
    let signed = fauna_mls::room_policy::RoomLabelers::new(channel, 1, [labeler])
        .sign(&admin)
        .expect("signs");

    assert_eq!(
        seen(None, true).await,
        Some(Vec::new()),
        "a community room naming no labeler stages from an empty set"
    );
    assert_eq!(
        seen(encode(&signed), true).await,
        Some(vec![labeler.to_hex()]),
        "the verified set is what the room renders as reading it"
    );
    assert_eq!(
        seen(encode(&signed), false).await,
        None,
        "a room whose floor seats no home nest has no reader to label it"
    );

    let mut elsewhere = fauna_mls::room_policy::RoomLabelers::new([0x77; 32], 1, [labeler])
        .sign(&admin)
        .expect("signs");
    assert_eq!(
        seen(encode(&elsewhere), true).await,
        None,
        "a set signed for another room never renders here"
    );
    elsewhere.labelers.room_id = channel.to_vec();
    assert_eq!(
        seen(encode(&elsewhere), true).await,
        None,
        "and re-labelling its room breaks the signature it carries"
    );
}

/// The snapshot a seat's room renders, over its own thread with itself as the
/// one participant.
fn room_snapshot_of(seat: &RoomSeat) -> fauna_conversations::room::RoomSnapshot {
    let mut detail = seat
        .manager
        .thread_detail(seat.thread.clone())
        .expect("thread");
    detail.participants = vec![TypedAddress::Fauna {
        actor_id: seat.actor,
        handle: String::new(),
    }];
    RailBackend::room_state(seat.backend.as_ref(), &detail).expect("a snapshot")
}

/// One invitation as the scripted nest holds it pending.
fn pending_invite(
    invitee: u8,
    invitee_handle: Option<&str>,
    inviter: ActorId,
    still_acceptable: bool,
) -> fauna_conversations::backend::PendingRoomInvite {
    fauna_conversations::backend::PendingRoomInvite {
        invitee: ActorId([invitee; 32]),
        invitee_handle: invitee_handle.map(str::to_string),
        inviter,
        inviter_handle: None,
        role: RoomRole::Member,
        invited_at_ms: 1_700_000_000_000 + i64::from(invitee),
        still_acceptable,
    }
}

/// **The invitations pending on a community room reach its snapshot, and a
/// withdrawal is on the screen without waiting out the floor cadence**
/// (`conversation-rooms.md` § Join rules and invites → *Pending invitations are
/// visible to whoever may withdraw them*).
///
/// The list rides the floor read, so it costs a room nothing it was not
/// already paying a poll for; the nest scopes it, so every row served is one
/// this viewer may withdraw and the snapshot carries no second gate. A lapsed
/// invitation is listed *as* lapsed — that is the whole point of listing it.
/// Withdrawing what is no longer pending is answered, not refused: the caller
/// wanted it gone and it is.
#[tokio::test]
async fn a_community_rooms_pending_invitations_reach_its_snapshot_and_a_withdrawal_re_lists() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let channel = ChannelId([0x5A; 32]);
    seat.backend.bind_channel(seat.thread.clone(), channel);
    seat_home_nest(&floor);
    *ceremony.pending_invites.lock().unwrap() = vec![
        pending_invite(0xB1, Some("bea@nest.example"), seat.actor, true),
        pending_invite(0xC1, None, ActorId([0xD1; 32]), false),
    ];
    assert_eq!(
        room_snapshot_of(&seat).pending_invites,
        None,
        "nothing is painted before the list has been read"
    );

    seat.backend
        .tend_community_room(&seat.manager, &channel)
        .await;

    let listed = room_snapshot_of(&seat)
        .pending_invites
        .expect("the list rides the floor read");
    assert_eq!(listed.len(), 2, "every row the nest served: {listed:?}");
    assert_eq!(listed[0].invitee_actor_hex, ActorId([0xB1; 32]).to_hex());
    assert_eq!(listed[0].invitee_display, "bea@nest.example");
    assert_eq!(listed[0].role, RoomRole::Member);
    assert!(!listed[0].lapsed);
    assert!(
        listed[1].lapsed,
        "an invitation the accept door would refuse is listed as lapsed"
    );
    assert_ne!(
        listed[1].invitee_display, "",
        "a handle-less invitee is named by its elided id, never blank"
    );

    seat.manager
        .withdraw_room_invite(seat.thread.clone(), ActorId([0xB1; 32]).to_hex())
        .await;
    assert_eq!(
        seat.manager.snapshot().error,
        None,
        "the owner withdraws an invitation"
    );
    assert_eq!(
        ceremony.revokes.lock().unwrap().as_slice(),
        &[(channel.to_string(), ActorId([0xB1; 32]).to_hex())],
        "the withdrawal names the room and the invitee"
    );
    let after = room_snapshot_of(&seat)
        .pending_invites
        .expect("re-listed by the withdrawal itself");
    assert_eq!(after.len(), 1, "the withdrawn row is gone: {after:?}");
    assert_eq!(after[0].invitee_actor_hex, ActorId([0xC1; 32]).to_hex());

    RailBackend::withdraw_room_invite(
        seat.backend.as_ref(),
        seat.thread.clone(),
        ActorId([0xB1; 32]),
    )
    .await
    .expect("withdrawing what is not pending is answered, not refused");

    // A refusal is the page's to show, and the row it was aimed at stays.
    *ceremony.refuse_revoke.lock().unwrap() = Some("not yours to withdraw".to_string());
    seat.manager
        .withdraw_room_invite(seat.thread.clone(), ActorId([0xC1; 32]).to_hex())
        .await;
    let error = seat
        .manager
        .snapshot()
        .error
        .expect("the refusal lands on the page's error-message");
    assert_eq!(
        error.key, "conversations.unified.error_withdraw_room_invite",
        "{error:?}"
    );
    assert_eq!(
        room_snapshot_of(&seat).pending_invites.map(|p| p.len()),
        Some(1),
        "a refused withdrawal removes nothing from the screen"
    );
}

/// **A list this device was not served paints nothing** — `None`, never an
/// empty list dressed up as an answer. Two rooms reach here: one whose nest
/// refuses the read (this account is off the floor, or not admitted to the
/// door), and one homed on another nest (the doors have no relay, so
/// asking this account's own nest would be asking the wrong floor).
#[tokio::test]
async fn a_pending_invitation_list_this_device_was_not_served_paints_nothing() {
    let nest = Arc::new(MockNest::default());

    let (seat, ceremony, floor) = policy_seat(&nest, "refused-lister");
    let channel = ChannelId([0x5A; 32]);
    seat.backend.bind_channel(seat.thread.clone(), channel);
    seat_home_nest(&floor);
    *ceremony.pending_invites.lock().unwrap() = vec![pending_invite(0xB1, None, seat.actor, true)];
    seat.backend
        .tend_community_room(&seat.manager, &channel)
        .await;
    assert!(room_snapshot_of(&seat).pending_invites.is_some());
    // The nest stops serving it: what was listed is not kept on the screen as
    // if it still stood.
    *ceremony.refuse_list_invites.lock().unwrap() = Some("not a member".to_string());
    for _ in 0..=FaunaMlsBackend::FLOOR_REFRESH_POLLS {
        seat.backend
            .tend_community_room(&seat.manager, &channel)
            .await;
    }
    assert_eq!(room_snapshot_of(&seat).pending_invites, None);

    let (seat, ceremony, floor) = policy_seat(&nest, "foreign-lister");
    let channel = ChannelId([0x5B; 32]);
    seat.backend.bind_channel(seat.thread.clone(), channel);
    seat.backend
        .record_channel_home(channel, "https://home.example");
    seat_home_nest(&floor);
    *ceremony.pending_invites.lock().unwrap() = vec![pending_invite(0xB1, None, seat.actor, true)];
    seat.backend
        .tend_community_room(&seat.manager, &channel)
        .await;
    assert_eq!(
        room_snapshot_of(&seat).pending_invites,
        None,
        "a room homed elsewhere is not asked of this account's own nest"
    );
}

/// **Naming a room's labelers signs the NEXT version of the stored set and
/// sends it through the governance door** — authored from what the nest
/// holds, never from what this device happens to know, for the policy's
/// reason: the version is the nest's ratchet, and a set built from a stale
/// local copy would be refused or would overwrite a co-admin's.
#[tokio::test]
async fn naming_a_rooms_labelers_signs_the_next_version_through_the_ceremony() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let channel = ChannelId([0x5A; 32]);
    seat_home_nest(&floor);
    let previous = seat
        .engine
        .sign_room_labelers(&fauna_mls::room_policy::RoomLabelers::new(
            channel.0,
            2,
            [ActorId([0x1A; 32])],
        ))
        .expect("signs");
    floor.holding_labelers(fauna_core::encoding::canonical_encode(&previous).unwrap());

    let (b, c) = (ActorId([0x1C; 32]), ActorId([0x1B; 32]));
    let version = seat
        .backend
        .set_room_labelers(&channel, vec![b, c])
        .await
        .expect("the owner names the room's labelers");

    assert_eq!(version, 3, "exactly one past the stored version");
    let sets = ceremony.labeler_sets.lock().unwrap();
    assert_eq!(sets.len(), 1, "one set through the door");
    assert_eq!(sets[0].signer, seat.actor, "signed as this device's actor");
    assert_eq!(sets[0].labelers.version, 3);
    assert_eq!(
        sets[0].labelers.labelers,
        vec![c, b],
        "the set is the canonical, sorted one the nest will verify"
    );
}

/// **A plain member naming labelers is refused HERE, in the policy editor's
/// words, before any round trip** — the nest would refuse anyway (rule 6,
/// "owner or admin for the rest"); this refusal is for the finger, and it is
/// the same one a plain member's policy edit gets.
#[tokio::test]
async fn a_plain_member_is_refused_naming_labelers_before_any_round_trip() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "member");
    for m in floor.members.lock().unwrap().iter_mut() {
        if m.actor == seat.actor {
            m.role = Some(fauna_conversations::room::RoomRole::Member);
        }
    }
    seat_home_nest(&floor);

    let err = seat
        .backend
        .set_room_labelers(&ChannelId([0x5A; 32]), vec![ActorId([0x1B; 32])])
        .await
        .expect_err("a plain member does not choose what reads the room");
    assert!(
        matches!(err, fauna_conversations::backend::BackendError::Refusal(_)),
        "a product refusal, not a transport error: {err:?}"
    );
    assert!(
        ceremony.labeler_sets.lock().unwrap().is_empty(),
        "and nothing reached the nest"
    );
}

/// **An edit a plain member's role does not permit is refused HERE, with the
/// words the other class uses** — not left to come back as a transport error.
///
/// The nest enforces the roles table itself and would refuse anyway; this is
/// the refusal for the user's finger, and it exists so the two classes refuse
/// the same gesture the same way. A user told "only the owner appoints admins"
/// on one room and handed a generic failure on another is meeting two products.
#[tokio::test]
async fn an_edit_the_floor_does_not_permit_is_refused_with_the_shared_words() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "plain-member");
    // The floor seats this account as a plain member rather than the owner.
    {
        let mut members = floor.members.lock().unwrap();
        members[0].role = Some(fauna_conversations::room::RoomRole::Member);
    }
    let stored = fauna_mls::room_policy::RoomPolicy::initial(
        ActorId([0x01; 32]),
        Some("the commons".into()),
    );
    floor.holding_policy(&stored.sign(&ActorKeypair::generate()).unwrap_or_else(|_| {
        panic!("a policy signed by somebody else — this test never reads its fields")
    }));

    let err = format!(
        "{}",
        seat.backend
            .set_room_policy(
                &ChannelId([0x5A; 32]),
                RoomPolicyEdit::AppointAdmin(ActorId([0xAD; 32])),
            )
            .await
            .expect_err("a plain member does not appoint admins")
    );
    assert!(
        err.contains("owner"),
        "the refusal names the rank, in the shared string: {err}"
    );
    assert!(
        ceremony.policies.lock().unwrap().is_empty(),
        "and nothing was signed or sent"
    );
}

/// **A hand-over moves the owner field and nothing else, signed by the
/// OUTGOING owner** (`conversation-rooms.md` § Roles and authorization —
/// "the signer's role in the *previous* version is what decides whether it may
/// have changed the owner field, and at signing time that is still the owner").
///
/// Its own door rather than a field of `set_policy` because it moves the roster
/// row's role as well as the bytes — and the nest refuses an owner change on
/// the other door by name.
///
/// **The successor leaves the admin set** ("the next version; the successor
/// leaves the admin set"), and that is structural rather than tidy: exactly one
/// owner exists and the owner is never also an admin, so a policy naming its
/// owner among the admins does not sign at all. Everyone else's rank survives —
/// including the outgoing owner's, who becomes a plain member and so becomes
/// removable like any other, which is the point of transferring rather than
/// leaving.
#[tokio::test]
async fn a_transfer_moves_the_owner_field_signed_by_the_outgoing_owner() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let successor = ActorId([0x50; 32]);
    let bystander = ActorId([0xBB; 32]);
    let mut stored =
        fauna_mls::room_policy::RoomPolicy::initial(seat.actor, Some("the commons".into()));
    stored.version = 2;
    stored.set_admins(vec![successor, bystander]);
    let channel = floor.holding_chain_to(&seat, &stored);

    let version = seat
        .backend
        .transfer_room_ownership(&channel, successor)
        .await
        .expect("the owner hands the room over");

    assert_eq!(version, 3);
    let (door, signed) = ceremony.stored_policy();
    assert_eq!(door, "transfer_ownership", "its own door, not set_policy");
    assert_eq!(signed.policy.owner, successor, "the room has a new owner");
    assert_eq!(
        signed.signer, seat.actor,
        "signed by the OUTGOING owner — the role that authorizes the change is the one it held \
         in the previous version"
    );
    assert_eq!(
        signed.policy.name.as_deref(),
        Some("the commons"),
        "and nothing else moved"
    );
    assert_eq!(
        signed.policy.admins,
        vec![bystander],
        "the successor left the admin set — an owner is never also an admin, and a policy \
         naming it in both would not sign — while every other rank survived"
    );
}

/// **A policy the reader cannot verify is no policy at all** — and both doors
/// refuse rather than amending it.
///
/// The bytes crossed a nest boundary, and they name an owner, an admin set and
/// a room's name; amending an unverified record would let the delivery path put
/// a rank in somebody's mouth, and *storing* the amendment would launder it
/// into the room's history under this account's own signature.
///
/// The refusal reads the same as a policy-less room's, which is honest: in both
/// cases this device knows of no policy it can stand behind.
#[tokio::test]
async fn a_policy_that_does_not_verify_is_not_amendable() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let mut forged = seat
        .engine
        .sign_room_policy(&fauna_mls::room_policy::RoomPolicy::initial(
            seat.actor,
            Some("the commons".into()),
        ))
        .expect("signs");
    forged.policy.name = Some("the annexe".into()); // signed over the old bytes
    floor.holding_policy(&forged);

    assert!(
        seat.backend
            .read_room_policy(&ChannelId([0x5A; 32]))
            .await
            .expect("a bad record is not a transport error")
            .is_none(),
        "a policy whose signature does not cover its bytes is not surfaced"
    );
    for err in [
        format!(
            "{}",
            seat.backend
                .set_room_policy(&ChannelId([0x5A; 32]), RoomPolicyEdit::Rename("x".into()))
                .await
                .expect_err("nothing to amend")
        ),
        format!(
            "{}",
            seat.backend
                .transfer_room_ownership(&ChannelId([0x5A; 32]), ActorId([0x50; 32]))
                .await
                .expect_err("nothing to amend")
        ),
    ] {
        assert!(err.contains("nothing to"), "{err}");
    }
    assert!(
        ceremony.policies.lock().unwrap().is_empty(),
        "and nothing reached the nest — an unverified record must not be laundered into the \
         room's history under this account's signature"
    );
}

/// **A device amends — and renders — only a current policy it has proven to be
/// this room's** (`conversation-rooms.md` § Roles and authorization → *A
/// fetched version is anchored, never believed*).
///
/// The floor is the party that serves the current policy, so its word for
/// which room those bytes belong to is no check. Two forgeries, each one the
/// signature alone does not catch:
///
/// - **the splice**: as this room's version 2, a version the owner genuinely
///   signed in ANOTHER room it founded — appointing a stranger — with its room
///   signature stripped. Every signature on it is valid; only the rule
///   that every room requires a room signature above version 1 refuses it. The owner's next
///   rename would sign the stranger into this room under a valid room
///   signature for it;
/// - **the off-chain version**: a version 2 carrying a valid room signature
///   for THIS room, signed by a key no version of it ever ranked.
///
/// Both read as no policy, both doors sign nothing, and the editor's policy
/// (the join rule and history policy it seeds from) reads none.
#[tokio::test]
async fn a_current_policy_the_device_has_not_anchored_is_neither_amended_nor_rendered() {
    use fauna_mls::room_policy::{RoomPolicy, binding_birth_salt, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let birth = RoomPolicy::initial(seat.actor, Some("square".into()));
    let channel = floor.holding_chain_to(&seat, &birth);
    let stranger = ActorKeypair::generate();
    let mut appointed = birth.clone();
    appointed.version = 2;
    appointed.set_admins([stranger.actor_id()]);

    let elsewhere = derive_room_id(&seat.actor, &binding_birth_salt(&[0x61; 24])).unwrap();
    let mut stripped = seat
        .engine
        .sign_room_policy_community(&elsewhere, &appointed)
        .expect("the owner signs in its other room");
    stripped.room_signature = None;
    let mut self_appointed = birth.clone();
    self_appointed.version = 2;
    self_appointed.set_admins([stranger.actor_id()]);
    let off_chain = self_appointed
        .sign_community(&channel.0, &stranger)
        .unwrap();

    for (arm, forged) in [("splice", stripped), ("off-chain", off_chain)] {
        floor.holding_policy(&forged);
        {
            let mut versions = floor.versions.lock().unwrap();
            versions.truncate(1);
            versions.push(forged);
        }
        assert!(
            seat.backend
                .read_room_policy(&channel)
                .await
                .expect("a forged record is not a transport error")
                .is_none(),
            "{arm}: a current policy this device has not anchored is not this room's policy"
        );
        let err = format!(
            "{}",
            seat.backend
                .set_room_policy(&channel, RoomPolicyEdit::Rename("the annexe".into()))
                .await
                .expect_err("nothing this device can stand behind to amend")
        );
        assert!(err.contains("nothing to"), "{arm}: {err}");
        assert!(
            ceremony.policies.lock().unwrap().is_empty(),
            "{arm}: nothing was signed on the forged base — a rename would have signed its \
             admin set into this room under this room's own room signature"
        );

        seat.backend
            .resolve_nameless_members(&seat.manager, &channel)
            .await;
        let mut detail = seat
            .manager
            .thread_detail(seat.thread.clone())
            .expect("thread");
        detail.participants = vec![fauna_conversations::address::TypedAddress::Fauna {
            actor_id: seat.actor,
            handle: String::new(),
        }];
        assert!(
            RailBackend::room_state(seat.backend.as_ref(), &detail)
                .expect("a snapshot")
                .policy
                .is_none(),
            "{arm}: and the editor seeds nothing from it"
        );
    }
}

/// **One rail door serves both classes: `update_room_policy` on a COMMUNITY
/// room routes to the nest's policy doors.**
///
/// This is what the room-settings editor and the thread-header rename actually
/// call, on every app. Before it, the arm read the policy out of the MLS group
/// context — which a community room does not have — so the editor's every
/// gesture failed on exactly the class the editor was most needed for.
///
/// The branch is the same local signature the send path uses: **a bound channel
/// with no MLS group is a community room**, born by `room.create` rather than
/// `bootstrap_group`. No round trip, no cached class projection, and true on any
/// device that gets this far. What it buys is that no app learns the class at
/// all — one door, one edit vocabulary, two implementations underneath.
#[tokio::test]
async fn the_rail_policy_arm_routes_a_community_room_to_the_nest_doors() {
    let nest = Arc::new(MockNest::default());
    let (seat, ceremony, floor) = policy_seat(&nest, "owner");
    let stored =
        fauna_mls::room_policy::RoomPolicy::initial(seat.actor, Some("the commons".into()));
    floor.holding_chain_to(&seat, &stored);

    // The rail arm, exactly as an app calls it — no class named anywhere.
    RailBackend::update_room_policy(
        seat.backend.as_ref(),
        seat.thread.clone(),
        RoomPolicyEdit::Rename("the square".into()),
    )
    .await
    .expect("the owner renames a community room from the ordinary rail door");

    let (door, signed) = ceremony.stored_policy();
    assert_eq!(
        door, "set_policy",
        "it went to the nest's policy door, not into an MLS commit the room has no group for"
    );
    assert_eq!(signed.policy.name.as_deref(), Some("the square"));
    assert_eq!(
        signed.policy.version, 2,
        "the ratchet, not a fresh document"
    );

    // And the hand-over goes to its own door through the same arm.
    let successor = ActorId([0x50; 32]);
    floor.appending(&signed);
    RailBackend::update_room_policy(
        seat.backend.as_ref(),
        seat.thread.clone(),
        RoomPolicyEdit::TransferOwnership(successor),
    )
    .await
    .expect("the owner hands the room over from the same door");
    let policies = ceremony.policies.lock().unwrap();
    assert_eq!(policies.len(), 2);
    assert_eq!(
        policies[1].0, "transfer_ownership",
        "a transfer is its own door on this class too — it moves the roster row, not just bytes"
    );
    assert_eq!(policies[1].1.policy.owner, successor);
}

/// **The thread header names a community room's class — and says nothing false
/// before it can** (`conversation-rooms.md` § Architectural rules, rule 1: a
/// room's class is a function of its member set).
///
/// The class is a statement about **who can read**, painted on the header of
/// every app. A community room's reader set includes its home nest, and the
/// nest is on its *floor* but is not a participant any thread renders — so a
/// class derived from the participant list can never see it, and every
/// community room rendered as `end-to-end`. That is the worst direction to be
/// wrong in: it claims privacy the room does not have.
///
/// `room_state` is synchronous, so it cannot ask; and "no MLS group" alone
/// cannot answer, because an MLS room this device has not joined looks the same
/// locally. The floor settles it, cached by the poll pass that already reads
/// it. This walks all three states: before the read (the honest local answer),
/// after it (the true class, ranks and policy), and the transport-only case
/// riding the same derivation.
#[tokio::test]
async fn the_thread_header_names_a_community_rooms_class_once_its_floor_is_read() {
    let nest = Arc::new(MockNest::default());
    let (seat, _ceremony, floor) = policy_seat(&nest, "member");
    // The home nest, seated as an ordinary member — this is the whole
    // difference between a community room and an end-to-end one, and it is a
    // row a floor read serves and a participant list never can.
    floor.members.lock().unwrap().push(room_row(
        ActorId([0x0E; 32]),
        RoomPrincipalKind::Nest,
        0x0E,
        &GroupReceptionKeyRecord::mint(1_700_000_000_000),
    ));
    let stored =
        fauna_mls::room_policy::RoomPolicy::initial(seat.actor, Some("the commons".into()));
    let channel = floor.holding_chain_to(&seat, &stored);

    let detail = || {
        let mut d = seat
            .manager
            .thread_detail(seat.thread.clone())
            .expect("thread");
        d.participants = vec![fauna_conversations::address::TypedAddress::Fauna {
            actor_id: seat.actor,
            handle: String::new(),
        }];
        d
    };

    // (a) Before any floor read: the local answer, which cannot say `Community`
    // — and must not, since this device has learned nothing that would justify
    // it. No policy, no rank: an unread room is not a governed one.
    let before = RailBackend::room_state(seat.backend.as_ref(), &detail()).expect("a snapshot");
    assert_eq!(
        before.class,
        fauna_conversations::room::RoomClass::EndToEnd,
        "an unread room reports the class its participants imply, never a guess"
    );
    assert!(before.policy.is_none() && before.my_role.is_none());

    // The poll pass's read — the one this device already makes to name members.
    seat.backend
        .resolve_nameless_members(&seat.manager, &channel)
        .await;

    // (b) After it: the floor's own kinds decide, and the nest on the floor is
    // what makes the room a community.
    let after = RailBackend::room_state(seat.backend.as_ref(), &detail()).expect("a snapshot");
    assert_eq!(
        after.class,
        fauna_conversations::room::RoomClass::Community,
        "the home nest is on the floor, so the room says so on its header"
    );
    assert_eq!(
        after.members.len(),
        1,
        "and the member list stays index-parallel with the participants — the nest is not a \
         chip, which is exactly why the class could not be derived from this list"
    );
    assert_eq!(
        after.my_role,
        Some(fauna_conversations::room::RoomRole::Owner),
        "the viewer's rank comes off the floor for a room with no group context to hold it"
    );
    assert_eq!(
        after.policy.as_ref().map(|p| p.name.as_deref()),
        Some(Some("the commons")),
        "and so does the policy the editor renders"
    );
    assert!(
        after.is_governed(),
        "a community room is governed — its roles are enforced, at the floor"
    );
}

/// **A seated principal this build cannot name never lowers the class.**
///
/// An unrecognised principal kind is what an older app meets when a newer nest
/// seats something it has not heard of. The two ways of getting the class wrong
/// are not equal: naming a room more open than it is makes its members more
/// careful; naming it more private than it is is a privacy claim this build
/// cannot support. So `Other` derives transport-only — the mirror of the same
/// variant's treatment on the keying side, where it is not handed a key.
#[tokio::test]
async fn a_principal_this_build_cannot_name_never_lowers_the_class() {
    let nest = Arc::new(MockNest::default());
    let (seat, _ceremony, floor) = policy_seat(&nest, "member");
    let channel = ChannelId([0x5B; 32]);
    seat.backend.bind_channel(seat.thread.clone(), channel);
    {
        let mut members = floor.members.lock().unwrap();
        members[0].kind = fauna_conversations::backend::RoomPrincipalKind::Other;
    }
    seat.backend
        .resolve_nameless_members(&seat.manager, &channel)
        .await;

    let mut detail = seat
        .manager
        .thread_detail(seat.thread.clone())
        .expect("thread");
    detail.participants = vec![fauna_conversations::address::TypedAddress::Fauna {
        actor_id: seat.actor,
        handle: String::new(),
    }];
    assert_eq!(
        RailBackend::room_state(seat.backend.as_ref(), &detail)
            .expect("a snapshot")
            .class,
        fauna_conversations::room::RoomClass::TransportOnly,
        "a seated principal this build cannot name is a reader of unknown character — the class \
         says so rather than claiming a privacy it cannot support"
    );
}

/// **An invitation that does not verify is DROPPED, not surfaced** — and it
/// does not hide the good ones beside it.
///
/// A delivered invitation names a room and an inviter, so rendering one before
/// its bytes are proven would let the delivery path put words in a principal's
/// mouth: "‹someone you trust› invited you to ‹a room›" is a sentence an
/// unverified record must never be allowed to write. The nest binds an
/// invitation's signer to the authenticated caller on the way in, but this
/// record is what *crossed* the nest boundary, and it is signed precisely so
/// that crossing does not have to be trusted.
///
/// Dropping rather than erroring is the other half: one forged record beside
/// three real ones must not cost the user the three. The standing truth is the
/// home nest's own invitation row, so a genuine invitation survives a
/// re-delivery while a forged one has nothing behind it.
#[tokio::test]
async fn an_invitation_that_does_not_verify_is_dropped_and_does_not_hide_the_rest() {
    let nest = Arc::new(MockNest::default());
    let bob = founder_seat(&nest, "bob");
    let alice = ActorKeypair::generate();
    let room = ChannelId([0x5A; 32]);
    let ceremony = ScriptedCeremony::new();

    let genuine = fauna_mls::room_policy::RoomInvite {
        room_id: room.0.to_vec(),
        invitee: bob.actor,
        role: fauna_mls::room_policy::RoomRole::Member,
        policy_version: 3,
    }
    .sign(&alice)
    .expect("alice signs her own invitation");

    // Three ways a delivered record can fail to be what it claims, each staged
    // ahead of the genuine one so a "stop at the first bad record" reader fails
    // here: bytes that are not an invitation at all; a signature that does not
    // verify; and — the sharp one — a *valid* signature over DIFFERENT bytes,
    // i.e. an attacker splicing a real inviter's name onto a room of its own
    // choosing.
    let mut forged_signature = genuine.clone();
    forged_signature.signature = vec![0u8; 64];
    let mut spliced = genuine.clone();
    spliced.invite.room_id = [0x99; 32].to_vec();
    let staged: Vec<Vec<u8>> = vec![
        vec![0x00, 0x01, 0x02],
        fauna_core::encoding::canonical_encode(&forged_signature).unwrap(),
        fauna_core::encoding::canonical_encode(&spliced).unwrap(),
        fauna_core::encoding::canonical_encode(&genuine).unwrap(),
    ];
    {
        let mut deliveries = ceremony.deliveries.lock().unwrap();
        for (i, bytes) in staged.into_iter().enumerate() {
            deliveries.push((i as i64 + 1, bytes));
        }
    }
    bob.backend.set_room_ceremony(ceremony.clone());

    let pending = bob
        .backend
        .pending_room_invitations()
        .await
        .expect("a malformed record is not a transport error");

    assert_eq!(
        pending.len(),
        1,
        "only the record that verifies under the inviter it names is surfaced"
    );
    assert_eq!(pending[0].id, 4, "the genuine one, behind all three");
    assert_eq!(pending[0].room_id, room.0);
    assert_eq!(pending[0].inviter, alice.actor_id());
    assert_eq!(pending[0].policy_version, 3);
    assert_eq!(
        pending[0].room_id_hex(),
        hex::encode(room.0),
        "the hex form every id-taking door and rendered surface uses is derived once, here"
    );
}

/// **A re-invitation after a removal is offered, not silently swallowed**. `room_invitations` used to settle any
/// standing invitation whose room id was bound to a thread on this device, on
/// the reasoning that "an invitation into a room this device already sits in
/// is spent" — but a removed member keeps that binding (the device keeps its
/// bubbles), so a legitimate re-invitation was consumed by the next
/// background sweep before the user ever saw it.
///
/// Mutation: read `self.thread_for_channel(&channel_id).is_some()` alone
/// (drop the `!self.confirmed_unseated(&channel_id)` half) and this reds —
/// the invitation is settled again instead of offered.
#[tokio::test]
async fn a_re_invitation_after_a_removal_is_offered_not_settled() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x7Cu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());

    // A CONFIRMED "not a member" read — alice's own removal.
    alice
        .backend
        .set_room_roster_reader(Arc::new(ScriptedReader::new(vec![None])));
    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    // The re-invitation, signed by some other member, delivered to alice —
    // exactly what the nest's own replay guard already allows post-removal
    // (`accept_room_invite`/`unseat_room_member` are unrelated to this read).
    let inviter = ActorKeypair::generate();
    let invite = fauna_mls::room_policy::RoomInvite {
        room_id: channel.0.to_vec(),
        invitee: alice.actor,
        role: fauna_mls::room_policy::RoomRole::Member,
        policy_version: 1,
    }
    .sign(&inviter)
    .expect("the inviter signs the re-invitation");
    ceremony
        .deliveries
        .lock()
        .unwrap()
        .push((1, fauna_core::encoding::canonical_encode(&invite).unwrap()));

    let standing = alice
        .backend
        .room_invitations()
        .await
        .expect("room_invitations ok");

    assert_eq!(
        standing.len(),
        1,
        "a re-invitation after a confirmed removal must be offered, not swallowed"
    );
    assert!(
        ceremony
            .deliveries
            .lock()
            .unwrap()
            .iter()
            .any(|(id, _)| *id == 1),
        "and not settled out from under the user before they ever see it"
    );
}

/// **The common path this fix must not touch: a standing invitation into a
/// room this device is still LIVE in is still spent.** The settle-quietly
/// behavior predates  and covers the ordinary case — an accept whose
/// settle round trip did not land — which must keep working unchanged.
#[tokio::test]
async fn a_standing_invitation_into_a_room_still_seated_in_is_still_settled() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x7Du8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA2);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());

    // A confirmed floor that still seats alice.
    let alice_row = room_row(alice.actor, RoomPrincipalKind::User, 0xA2, &alice.reception);
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![alice_row]));
    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    let inviter = ActorKeypair::generate();
    let invite = fauna_mls::room_policy::RoomInvite {
        room_id: channel.0.to_vec(),
        invitee: alice.actor,
        role: fauna_mls::room_policy::RoomRole::Member,
        policy_version: 1,
    }
    .sign(&inviter)
    .expect("the inviter signs the stale invitation");
    ceremony
        .deliveries
        .lock()
        .unwrap()
        .push((1, fauna_core::encoding::canonical_encode(&invite).unwrap()));

    let standing = alice
        .backend
        .room_invitations()
        .await
        .expect("room_invitations ok");

    assert!(
        standing.is_empty(),
        "still seated: the stale invitation must settle quietly, not be offered again"
    );
    assert!(
        ceremony.deliveries.lock().unwrap().is_empty(),
        "and it must actually be settled, not merely hidden"
    );
}

/// **A device that holds no key for the room's tip cannot key anybody in.**
///
/// The refusal is the honest one: a backfill hands out a generation key, so a
/// device that cannot open the tip has nothing to hand out. Without this the
/// failure would surface as a nest-side batch refusal — after a round trip,
/// naming the *wraps* rather than the fact that this device was never keyed
/// into the room it is trying to key somebody else into.
///
/// The second half is the same rule from the other side: an *invited* but
/// un-accepted principal is not on the floor, holds no roster entry, and so
/// cannot be wrapped to at all. An invitation is not a seating.
#[tokio::test]
async fn keying_a_member_in_needs_the_tip_and_a_seated_target() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let stranger = ActorId([0x33; 32]);
    let ceremony = ScriptedCeremony::new();
    let roster = Arc::new(ScriptedReader::new(vec![
        Some(vec![room_row(
            alice.actor,
            RoomPrincipalKind::User,
            0xA1,
            &alice.reception,
        )]),
        Some(vec![room_row(
            alice.actor,
            RoomPrincipalKind::User,
            0xA1,
            &alice.reception,
        )]),
    ]));
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(roster);
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));
    let channel = alice
        .backend
        .found_community_room(alice.thread.clone(), None)
        .await
        .expect("alice founds the room");

    // (a) No generation reader registered — this device cannot resolve the tip.
    let err = alice
        .backend
        .key_in_room_member(&channel, stranger)
        .await
        .expect_err("a device with no key for the tip hands out nothing");
    assert!(
        format!("{err}").contains("holds no key"),
        "the refusal names the missing key rather than the target: {err}"
    );

    // (b) With the tip in hand, a principal the floor does not seat is still
    //     refused — before any round trip.
    let (generation_id, wraps, commitment, _) = ceremony.published();
    alice
        .backend
        .set_room_generation_reader(Arc::new(ScriptedGenerations::new(vec![
            RoomGenerationWrap {
                generation_id,
                key_commitment: commitment,
                wrap: wraps
                    .iter()
                    .find(|w| w.entry_id == [0xA1; 32])
                    .unwrap()
                    .wrap
                    .clone(),
                entry_id: [0xA1; 32],
                is_tip: true,
            },
        ])));
    let err = alice
        .backend
        .key_in_room_member(&channel, stranger)
        .await
        .expect_err("an invited-but-unaccepted principal holds no roster entry to wrap to");
    assert!(
        format!("{err}").contains("not a keyable member"),
        "the refusal names the missing seating: {err}"
    );
    assert!(
        ceremony.backfills.lock().unwrap().is_empty(),
        "and nothing reached the nest — the check is local, on the floor this device just read"
    );
}

/// **The class works end to end: two seats of a community room exchange a
/// message, and the reader learns who wrote it.**
///
/// Everything here is the community path and nothing is MLS: neither engine
/// holds a group for this channel, the body seals under the room's generation
/// key, and it rides the ordinary `channel.send` in a `RoomSealed` envelope —
/// "one storage shape and one read feed for both classes, differing only in
/// which key the reader holds" (`conversation-rooms.md` § The three classes →
/// *Community*).
#[tokio::test]
async fn two_seats_of_a_community_room_exchange_an_attributed_message() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2au8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    assert!(
        !alice.engine.has_group(&channel) && !bob.engine.has_group(&channel),
        "a community room has NO MLS group — that is what makes it the class it is"
    );

    let gen_key = GenerationKey::mint();
    let generation_id = [0x5au8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "the square is open".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("a keyed seat can send into its room");

    let envelopes = nest.sent_envelopes(&channel.to_string());
    assert_eq!(envelopes.len(), 1, "exactly one channel.send");
    assert!(
        matches!(
            ChannelEnvelope::from_bytes(&envelopes[0]).expect("decode"),
            ChannelEnvelope::RoomSealed { .. }
        ),
        "a community send rides the RoomSealed variant, not an MLS application \
         message — no new send kind"
    );

    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let detail = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread");
    let bubble = detail
        .messages
        .iter()
        .find(|m| m.body == "the square is open")
        .expect("the message opened into bob's thread");
    assert_eq!(
        bubble.sender.person_actor_id(),
        Some(alice.actor),
        "THE assertion: the bubble names its author, and the author is a signed \
         claim rather than anything the nest or the key asserted"
    );

    // The cursor holds, exactly as on the MLS path.
    assert_eq!(
        bob.poll(&channel, &mut after_seq).await,
        0,
        "cursor prevents re-ingest"
    );
}

/// The community arm's pre-open skip asks the room's OWN thread, like the MLS
/// arm's (`a_message_id_held_by_another_thread_never_skips_a_channels_own_record`):
/// an id another thread on this device was handed never steps the walk over a
/// record of this room unread.
#[tokio::test]
async fn a_message_id_held_by_another_thread_never_skips_a_community_rooms_record() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2bu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5bu8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "seed".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("send ok");
    // A third seat reads it, for a real snapshot to shape the planted entry from.
    let carol = room_seat(&nest, channel, "carol", 3);
    carol.hold(&gen_key, generation_id);
    let mut carol_seq = 0i64;
    carol.poll(&channel, &mut carol_seq).await;
    let template = carol
        .manager
        .thread_detail(carol.thread.clone())
        .unwrap()
        .messages
        .first()
        .expect("carol read the message")
        .clone();

    // Another thread on bob's device holds this room's id for seq 1.
    let erin = fauna_addr("erin", ActorKeypair::generate().actor_id());
    let mut replica = empty_channel_slice(&hex::encode([8u8; 32]), "other", erin.clone());
    replica.messages = vec![carried(
        &template,
        format!("conv:{channel}:1"),
        erin,
        "held elsewhere",
    )];
    bob.manager.restore_channel_slice(&replica);

    let mut after_seq = 0i64;
    assert_eq!(
        bob.poll(&channel, &mut after_seq).await,
        1,
        "the room's own record opens and folds"
    );
}

/// **A community room carries the sender's own delete and reactions — on the
/// sealed path, judged by the signed author.**
///
/// `conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism* → *Community rooms*, the first act: a `Delete` (and a
/// `Reaction`) body rides the same `RoomSealed` envelope every community
/// message rides, its sender the **signed author**; members apply the sender
/// match, so a plain member's sealed delete of another member's message is the
/// forged delete it is in the end-to-end class — dropped on every seat. The
/// floor never sees which of these a send was.
#[tokio::test]
async fn a_community_rooms_own_delete_and_reactions_ride_the_sealed_path() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2cu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let carol = room_seat(&nest, channel, "carol", 3);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5cu8; 32];
    for seat in [&alice, &bob, &carol] {
        seat.hold(&gen_key, generation_id);
    }
    let say = |seat: &RoomSeat, text: &str| {
        let backend = seat.backend.clone();
        let thread = fauna_mls_thread(seat.thread.clone(), vec![]);
        let compose = ComposeState {
            body_draft: text.into(),
            ..Default::default()
        };
        async move {
            backend
                .send(&thread, &compose, &[])
                .await
                .expect("a keyed seat can send into its room");
        }
    };
    say(&alice, "alice one").await; // seq 1
    say(&bob, "bob two").await; // seq 2
    let hex = channel.to_string();
    let one = MessageId(format!("conv:{hex}:1"));
    let two = MessageId(format!("conv:{hex}:2"));

    let (mut a, mut b, mut c) = (0i64, 0i64, 0i64);
    alice.poll(&channel, &mut a).await;
    bob.poll(&channel, &mut b).await;
    carol.poll(&channel, &mut c).await;
    let bubble = |seat: &RoomSeat, id: &MessageId| {
        seat.manager
            .thread_detail(seat.thread.clone())
            .expect("thread")
            .messages
            .into_iter()
            .find(|m| &m.message_id == id)
            .expect("the bubble is held")
    };

    // Bob reacts to alice's message; alice deletes her own; carol — a plain
    // member — seals a delete of bob's message straight onto the wire (her
    // manager would refuse the gesture, so the forgery goes under it).
    bob.manager
        .toggle_reaction(bob.thread.clone(), one.clone(), "👍".into())
        .await;
    carol.poll(&channel, &mut c).await;
    let reactions = bubble(&carol, &one).reactions;
    assert_eq!(
        reactions
            .iter()
            .map(|g| (g.emoji.as_str(), g.count))
            .collect::<Vec<_>>(),
        vec![("👍", 1)],
        "a reaction sealed under the room key folds on another member's seat, \
         attributed to its signed author"
    );

    alice
        .manager
        .delete_message(alice.thread.clone(), one.clone())
        .await;
    carol
        .backend
        .send_delete(&carol.thread, 2)
        .await
        .expect("the forged delete seals like any other send");
    assert!(
        nest.sent_envelopes(&hex).iter().all(|e| matches!(
            ChannelEnvelope::from_bytes(e).expect("decode"),
            ChannelEnvelope::RoomSealed { .. }
        )),
        "every one of these rides the RoomSealed variant — the floor cannot tell \
         a delete or a reaction from a message, and is never asked to"
    );

    alice.poll(&channel, &mut a).await;
    bob.poll(&channel, &mut b).await;
    carol.poll(&channel, &mut c).await;
    assert!(
        bubble(&bob, &one).deleted && bubble(&carol, &one).deleted,
        "the sender's own delete tombstones her message on the other seats"
    );
    assert!(
        !bubble(&alice, &two).deleted && !bubble(&bob, &two).deleted,
        "a plain member's sealed delete of another member's message is dropped \
         as forged — the sender match is the signed author's, never loosened"
    );
}

/// ⚠ **A community room's reaction op is replayable, and the fold must not
/// care.** The author's signature covers `(room, generation, author,
/// sent_at_ms, body)` but never the log `seq` — the nest allocates that after
/// the author signs (`community-rooms.md` § The three classes → *Community* →
/// *Who wrote it*) — so the identical envelope bytes, re-appended by any
/// member or by the home nest that holds the log, open and verify as that
/// author's op again at a later position. No key, no re-seal, no forged
/// signature: the bytes go back verbatim.
///
/// Bob adds 👎 and then retracts it. A hostile party re-appends the ADD's own
/// bytes after the retraction. Under a fold that took the last op in log
/// order, that put Bob's reaction back on every seat under his own valid
/// signature; under the stamp-ordered fold
/// (`fauna_conversations::fold_reactions`) the outcome reads the SET of signed
/// ops, so re-appending one the log already holds changes nothing.
///
/// The second leg is the one that decides whether the property is real or
/// merely usually-true: it replays the *retraction* instead, to show the rule
/// is not "the newest bytes win" but "the set decides".
#[tokio::test]
async fn a_replayed_reaction_op_cannot_override_a_later_retraction() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2du8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5du8; 32];
    for seat in [&alice, &bob] {
        seat.hold(&gen_key, generation_id);
    }
    let hex = channel.to_string();

    // seq 1: alice's message, the reaction target.
    let thread = fauna_mls_thread(alice.thread.clone(), vec![]);
    alice
        .backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "alice one".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("a keyed seat can send into its room");
    let one = MessageId(format!("conv:{hex}:1"));

    let (mut a, mut b) = (0i64, 0i64);
    alice.poll(&channel, &mut a).await;
    bob.poll(&channel, &mut b).await;

    // seq 2: bob adds 👎. seq 3: bob retracts it.
    bob.manager
        .toggle_reaction(bob.thread.clone(), one.clone(), "👎".into())
        .await;
    bob.manager
        .toggle_reaction(bob.thread.clone(), one.clone(), "👎".into())
        .await;
    alice.poll(&channel, &mut a).await;

    let pills = |seat: &RoomSeat| -> Vec<(String, u32)> {
        seat.manager
            .thread_detail(seat.thread.clone())
            .expect("thread")
            .messages
            .into_iter()
            .find(|m| m.message_id == one)
            .expect("the target bubble is held")
            .reactions
            .into_iter()
            .map(|g| (g.emoji, g.count))
            .collect()
    };
    assert!(
        pills(&alice).is_empty() && pills(&bob).is_empty(),
        "the honest log: bob's retraction stands on both seats"
    );

    let sent = nest.sent_envelopes(&hex);
    assert_eq!(sent.len(), 3, "the message, the add, the retraction");

    // The ADD's own bytes, re-appended as seq 4 — no key, no re-seal, no new
    // signature. Exactly what any member or the home nest can do.
    nest.push_envelope(&hex, sent[1].clone());
    alice.poll(&channel, &mut a).await;
    bob.poll(&channel, &mut b).await;
    assert!(
        pills(&alice).is_empty(),
        "a replayed add must not resurrect a retracted reaction on a peer's seat; \
         got {:?}",
        pills(&alice)
    );
    assert!(
        pills(&bob).is_empty(),
        "nor on the reactor's own seat, which folds the same log; got {:?}",
        pills(&bob)
    );

    // And the mirror: replaying the RETRACTION is a no-op too. The rule is
    // that the set of signed ops decides, not that the last bytes appended win.
    nest.push_envelope(&hex, sent[2].clone());
    alice.poll(&channel, &mut a).await;
    assert!(
        pills(&alice).is_empty(),
        "replaying the retraction changes nothing either"
    );
}

/// A community room's floor as one seat's roster reader serves it, **by policy
/// version**: the live roster, the signed versions the home nest retains, and
/// the room's birth salt. Each seat holds its own, so a test can hand one seat
/// a floor its nest forged while the others read the honest one.
struct VersionedFloor {
    members: Vec<RoomRosterKnownMember>,
    versions: Vec<fauna_mls::room_policy::SignedRoomPolicy>,
    salt: [u8; 32],
    /// `false` = the versioned read reaches no answer this pass.
    reachable: Mutex<bool>,
}

#[async_trait]
impl RoomRosterReader for VersionedFloor {
    async fn read_roster(&self, _channel: String, _home: Option<String>) -> RoomRosterRead {
        RoomRosterRead::Floor(RoomFloor {
            members: self.members.clone(),
            policy_version: Some(self.versions.len() as u64),
            policy: self
                .versions
                .last()
                .map(|signed| fauna_core::encoding::canonical_encode(signed).unwrap()),
            labelers: None,
        })
    }

    async fn read_policy_version(
        &self,
        _channel: String,
        _home: Option<String>,
        version: u64,
    ) -> fauna_conversations::backend::RoomPolicyVersionRead {
        use fauna_conversations::backend::RoomPolicyVersionRead;
        if !*self.reachable.lock().unwrap() {
            return RoomPolicyVersionRead::Unavailable;
        }
        match self.versions.get((version as usize).wrapping_sub(1)) {
            Some(signed) => RoomPolicyVersionRead::Served {
                policy: fauna_core::encoding::canonical_encode(signed).unwrap(),
                birth_salt: Some(self.salt),
            },
            None => RoomPolicyVersionRead::NotHeld,
        }
    }
}

/// **An owner's or admin's delete of another member's message tombstones it on
/// the other seats of a community room — and nothing else does, the room's own
/// home nest included.**
///
/// `conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism* → *Community rooms*, the second act and *Members verify what
/// they paint*: the delete is a signed, unsealed **floor delete record**; a
/// member honours it only when its author is owner or admin in the policy **of
/// the version the record names**, and only a version this member has anchored
/// itself — version 1 to the room id, each later one to the one before. So:
///
/// - the owner's record (sent through the manager's delete gesture) and the
///   admin's tombstone their targets on a plain member's seat;
/// - a plain member's record, filed under the floor, is dropped;
/// - a record naming a version the room never held is dropped;
/// - **the forgery the anchor exists for**: a home nest that mints a key, serves
///   a policy naming it, and appends that key's record paints nothing — whether
///   the forged policy is a version 1 or a later link;
/// - **the splice the room signature exists for**: a home nest that serves, as
///   this room's version 2, one the owner genuinely signed in ANOTHER room it
///   founded — appointing a stranger to this room — paints that stranger's
///   record nothing, whether the version keeps its room signature or the nest
///   strips it (every room requires one above version 1);
/// - a version that cannot be fetched *yet* paints nothing, and the tombstone
///   lands on the pass after it can.
#[tokio::test]
async fn a_community_floor_delete_is_honoured_only_under_a_policy_the_member_anchored() {
    use fauna_conversations::room::RoomRole;
    use fauna_mls::room_policy::{RoomPolicy, binding_birth_salt, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = binding_birth_salt(&[0x51u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (owner_key, minted_key) = (key(0xa1), key(0xee));
    let channel = ChannelId(derive_room_id(&owner_key.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let owner = room_seat_as(&nest, channel, "owner", 1, key(0xa1));
    let admin = room_seat_as(&nest, channel, "admin", 2, key(0xa2));
    let carol = room_seat(&nest, channel, "carol", 3);
    let bob = room_seat(&nest, channel, "bob", 4);
    // Four more plain members, each of whose home nest forges its floor.
    let dave = room_seat(&nest, channel, "dave", 5);
    let erin = room_seat(&nest, channel, "erin", 6);
    let frank = room_seat(&nest, channel, "frank", 7);
    let grace = room_seat(&nest, channel, "grace", 8);
    let gen_key = GenerationKey::mint();
    for seat in [&owner, &admin, &carol, &bob, &dave, &erin, &frank, &grace] {
        seat.hold(&gen_key, [0x5du8; 32]);
    }

    // The honest chain: the founder's version 1, then the owner appoints the
    // admin.
    let v1 = RoomPolicy::initial(owner.actor, Some("square".into()))
        .sign_community(&channel.0, &owner_key)
        .unwrap();
    let mut appointed = v1.policy.clone();
    appointed.version = 2;
    appointed.set_admins([admin.actor]);
    let v2 = appointed.sign_community(&channel.0, &owner_key).unwrap();

    let row = |seat: &RoomSeat, role: RoomRole| RoomRosterKnownMember {
        role: Some(role),
        ..known(seat.actor, None)
    };
    let members = vec![
        row(&owner, RoomRole::Owner),
        row(&admin, RoomRole::Admin),
        row(&carol, RoomRole::Member),
        row(&bob, RoomRole::Member),
        RoomRosterKnownMember {
            kind: RoomPrincipalKind::Nest,
            ..known(ActorId([0x4e; 32]), None)
        },
    ];
    let floor = |versions: Vec<fauna_mls::room_policy::SignedRoomPolicy>| {
        Arc::new(VersionedFloor {
            members: members.clone(),
            versions,
            salt,
            reachable: Mutex::new(true),
        })
    };
    for seat in [&owner, &admin, &carol] {
        seat.backend
            .set_room_roster_reader(floor(vec![v1.clone(), v2.clone()]));
    }
    let bobs_floor = floor(vec![v1.clone(), v2.clone()]);
    *bobs_floor.reachable.lock().unwrap() = false;
    bob.backend.set_room_roster_reader(bobs_floor.clone());
    // Dave's nest forges a LATER link: the true birth record, then a version 2
    // the minted key signed, appointing itself. Erin's forges the birth record.
    let mut self_appointed = v1.policy.clone();
    self_appointed.version = 2;
    self_appointed.set_admins([minted_key.actor_id()]);
    dave.backend.set_room_roster_reader(floor(vec![
        v1.clone(),
        self_appointed
            .sign_community(&channel.0, &minted_key)
            .unwrap(),
    ]));
    erin.backend.set_room_roster_reader(floor(vec![
        RoomPolicy::initial(minted_key.actor_id(), None)
            .sign_community(&channel.0, &minted_key)
            .unwrap(),
    ]));
    // Frank's and grace's nests splice: the true birth record, then a version 2
    // the OWNER signed in another room it founded, appointing the minted key —
    // as signed (its room signature that other room's), and stripped of it.
    let elsewhere =
        derive_room_id(&owner_key.actor_id(), &binding_birth_salt(&[0x61; 24])).unwrap();
    let spliced = self_appointed
        .sign_community(&elsewhere, &owner_key)
        .unwrap();
    let mut stripped = spliced.clone();
    stripped.room_signature = None;
    frank
        .backend
        .set_room_roster_reader(floor(vec![v1.clone(), spliced]));
    grace
        .backend
        .set_room_roster_reader(floor(vec![v1.clone(), stripped]));

    // Bob says five things (seq 1–5); everybody reads them.
    for text in ["one", "two", "three", "four", "five"] {
        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: text.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
    }
    let id = |seq: u64| MessageId(format!("conv:{hex}:{seq}"));
    let mut cursors = [0i64; 8];
    let seats = [&owner, &admin, &carol, &bob, &dave, &erin, &frank, &grace];
    for (seat, cursor) in seats.iter().zip(cursors.iter_mut()) {
        seat.poll(&channel, cursor).await;
    }

    // seq 6 — the OWNER, through the gesture every app makes. The floor read
    // is what tells the manager this viewer governs the room.
    owner
        .backend
        .tend_community_room(&owner.manager, &channel)
        .await;
    owner
        .manager
        .delete_message(owner.thread.clone(), id(1))
        .await;
    // seq 7 — the ADMIN. seq 8 — CAROL, a plain member, under the floor (the
    // mock nest is a blind append; the real floor refuses her).
    for (seat, target) in [(&admin, 2), (&carol, 3)] {
        seat.backend
            .send_delete_any(&seat.thread, target)
            .await
            .expect("the record is filed");
    }
    // seq 9 — the admin again, naming a version the room never held.
    // seq 10, 11 — the MINTED key's records, naming the forged version 2 and
    // the forged version 1. seq 12 — the OWNER's genuine record for one
    // message, re-pointed at another after signing: the signature no longer
    // covers what it says.
    let minted = MlsEngine::new_in_memory(key(0xee)).unwrap();
    let repointed = owner
        .engine
        .sign_room_floor_delete(&channel.0, 4, 2)
        .map(|mut signed| {
            signed.record.target_seq = 5;
            signed
        });
    for record in [
        admin.engine.sign_room_floor_delete(&channel.0, 4, 9),
        minted.sign_room_floor_delete(&channel.0, 4, 2),
        minted.sign_room_floor_delete(&channel.0, 5, 1),
        repointed,
    ] {
        nest.push_envelope(
            &hex,
            ChannelEnvelope::RoomFloorDelete(record.unwrap().to_bytes().unwrap())
                .to_bytes()
                .unwrap(),
        );
    }
    let sent = nest.sent_envelopes(&hex);
    assert!(
        sent[5..].iter().all(|e| matches!(
            ChannelEnvelope::from_bytes(e).expect("decode"),
            ChannelEnvelope::RoomFloorDelete(_)
        )),
        "a cross-sender delete is the unsealed floor record — the manager's \
         gesture included — never a sealed `Delete` the floor could not judge"
    );

    for (seat, cursor) in seats.iter().zip(cursors.iter_mut()) {
        seat.poll(&channel, cursor).await;
    }
    let deleted = |seat: &RoomSeat, seq: u64| {
        seat.manager
            .thread_detail(seat.thread.clone())
            .expect("thread")
            .messages
            .into_iter()
            .find(|m| m.message_id == id(seq))
            .expect("the bubble is held")
            .deleted
    };
    let painted = |seat: &RoomSeat| {
        (1..=5)
            .filter(|seq| deleted(seat, *seq))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        painted(&carol),
        vec![1, 2],
        "the owner's and the admin's records tombstone their targets; a plain \
         member's, one naming a version the room never held, and the owner's \
         own record re-pointed after signing paint nothing"
    );
    assert_eq!(
        painted(&dave),
        Vec::<u64>::new(),
        "THE ANCHOR, a later link: dave's nest served a version 2 its own minted \
         key signed, appointing itself. A member under version 1 signs no \
         version 2, so the chain stops at the founder's — the minted key's \
         record paints nothing, and nor does any other record naming a version \
         dave could not prove (a withheld link fails closed)"
    );
    assert_eq!(
        painted(&erin),
        Vec::<u64>::new(),
        "THE ANCHOR, the birth record: erin's nest served a version 1 naming its \
         minted key owner. It does not derive the room id, so nothing anchors \
         and nothing is painted — the nest cannot forge a tombstone alone"
    );
    for (seat, name) in [(&frank, "frank"), (&grace, "grace")] {
        assert_eq!(
            painted(seat),
            Vec::<u64>::new(),
            "THE ROOM SIGNATURE: {}'s nest served, as this room's version 2, one \
             the owner signed in another room it founded, appointing the minted \
             key. Its room signature is that other room's, or gone, which no \
             room admits above version 1, so the chain stops at the founder's \
             — the minted key's record paints nothing",
            name
        );
    }

    // Bob's nest could not be reached for the version: nothing painted YET …
    assert_eq!(
        painted(&bob),
        Vec::<u64>::new(),
        "unverified paints nothing"
    );
    // … and the walk has stepped past the records, so the retry is what paints
    // them once the version can be fetched.
    *bobs_floor.reachable.lock().unwrap() = true;
    bob.poll(&channel, &mut cursors[3]).await;
    assert_eq!(
        painted(&bob),
        vec![1, 2],
        "a record parked for want of its policy version is judged again, and \
         painted, on the pass after the version arrives"
    );
}

/// **A floor delete record parked when the app quits is judged — and painted —
/// on the first pass after the relaunch, from the replica, not the log.**
///
/// `conversation-rooms.md` § Implementation status today, residual *(d)*, and
/// § Roles and authorization → *Delete any message — the mechanism* → *Members
/// verify what they paint*: the inbound walk advances the durable cursor past a
/// floor delete record BEFORE it is judged, and a record the version read
/// could not reach is parked in backend memory for a later pass. So a quit
/// between the park and that pass used to lose the record for good — the
/// resumed poll starts past it, and nothing re-walks it — leaving its target
/// painted on this account for ever while every other member shows it
/// deleted, silently (the room's unverified-moderation notice is derived off
/// the same parked set). The parked set now rides the `history/<ch>` slice
/// beside the tombstones (`ChannelHistorySlice::parked_floor_deletes`):
/// snapshotted by the manager's one slice door, re-seeded by its restore, and
/// drained by the backend into its live set on its first pass over the room.
///
/// The relaunch here is the real shape: a fresh engine, manager and backend
/// under the same identity, the slice the old session wrote restored into the
/// new manager, and the poll resumed from the OLD session's cursor — past the
/// records. Red-verified: with the at-rest field left empty, nothing paints.
#[tokio::test]
async fn a_floor_delete_record_parked_at_quit_is_judged_and_painted_after_the_relaunch() {
    use fauna_conversations::room::RoomRole;
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x52u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (owner_key, bob_key) = (key(0xa3), key(0xa4));
    let channel = ChannelId(derive_room_id(&owner_key.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let owner = room_seat_as(&nest, channel, "owner", 1, key(0xa3));
    let bob = room_seat_as(&nest, channel, "bob", 2, key(0xa4));
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5eu8; 32];
    owner.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    let v1 = RoomPolicy::initial(owner.actor, Some("square".into()))
        .sign_community(&channel.0, &owner_key)
        .unwrap();
    let row = |seat: &RoomSeat, role: RoomRole| RoomRosterKnownMember {
        role: Some(role),
        ..known(seat.actor, None)
    };
    let members = vec![row(&owner, RoomRole::Owner), row(&bob, RoomRole::Member)];
    let floor = || {
        Arc::new(VersionedFloor {
            members: members.clone(),
            versions: vec![v1.clone()],
            salt,
            reachable: Mutex::new(true),
        })
    };
    owner.backend.set_room_roster_reader(floor());
    let bobs_floor = floor();
    *bobs_floor.reachable.lock().unwrap() = false;
    bob.backend.set_room_roster_reader(bobs_floor.clone());

    // seq 1, 2 — bob says two things; both seats read them.
    for text in ["one", "two"] {
        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: text.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
    }
    let id = |seq: u64| MessageId(format!("conv:{hex}:{seq}"));
    let (mut owner_cursor, mut bob_cursor) = (0i64, 0i64);
    owner.poll(&channel, &mut owner_cursor).await;
    bob.poll(&channel, &mut bob_cursor).await;

    // seq 3 — the OWNER deletes bob's first message, through the gesture.
    owner
        .backend
        .tend_community_room(&owner.manager, &channel)
        .await;
    owner
        .manager
        .delete_message(owner.thread.clone(), id(1))
        .await;

    // Bob's nest cannot serve the version: the record parks, the cursor is
    // past it, nothing paints.
    bob.poll(&channel, &mut bob_cursor).await;
    let painted = |manager: &ConversationsManager, thread: &ThreadId| {
        manager
            .thread_detail(thread.clone())
            .expect("thread")
            .messages
            .into_iter()
            .filter(|m| m.deleted)
            .map(|m| m.message_id)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        painted(&bob.manager, &bob.thread),
        Vec::<MessageId>::new(),
        "unverified paints nothing"
    );
    assert_eq!(
        bob_cursor, 3,
        "and the walk has stepped past the record — the log will not hand it back"
    );

    // The old session writes its slice and quits.
    let slice = bob
        .manager
        .snapshot_channel_slice(&bob.thread, &hex, bob_cursor)
        .expect("bob's thread snapshots");
    assert_eq!(
        slice.parked_floor_deletes.len(),
        1,
        "the parked record rides the slice — the one door every writer takes"
    );
    let bytes = slice.to_bytes().unwrap();
    drop(bob);

    // The relaunch: a fresh engine, manager and backend under bob's identity,
    // the slice restored, the channel bound — and the version now reachable.
    let relaunched = room_seat_as(&nest, channel, "bob", 2, bob_key);
    relaunched.hold(&gen_key, generation_id);
    let restored = ChannelHistorySlice::from_bytes(&bytes).unwrap();
    let thread = relaunched.manager.restore_channel_slice(&restored);
    relaunched.backend.bind_channel(thread.clone(), channel);
    let floor_now = floor();
    relaunched.backend.set_room_roster_reader(floor_now);
    assert_eq!(
        painted(&relaunched.manager, &thread),
        Vec::<MessageId>::new(),
        "a restore alone paints nothing — the record is a claim, not a verdict"
    );

    // The first pass after the relaunch resumes from the OLD cursor — past
    // the record — and still judges it, off the replica.
    let mut cursor = bob_cursor;
    relaunched.poll(&channel, &mut cursor).await;
    assert_eq!(
        painted(&relaunched.manager, &thread),
        vec![id(1)],
        "a record parked at quit is judged, and painted, on the first pass after \
         the relaunch — the log never met it again; the replica carried it"
    );
    let again = relaunched
        .manager
        .snapshot_channel_slice(&thread, &hex, cursor)
        .expect("snapshots");
    assert!(
        again.parked_floor_deletes.is_empty(),
        "judged is judged: the next slice carries the tombstone, not the claim"
    );
}

/// The lines one seat's witness has **verified** — a stand-in for
/// `ChainWitness::resolve_line`, whose own anchoring is pinned in
/// `fauna-client-recovery`. A name it holds nothing for is
/// [`SuccessionLine::NotYet`], the no-anchor arm; [`Self::learn`] is the
/// harvest seeding an anchor later.
///
/// [`SuccessionLine::NotYet`]: fauna_conversations::backend::SuccessionLine
#[derive(Default)]
struct LineWitness {
    lines: Mutex<HashMap<ActorId, Vec<ActorId>>>,
    asked: Mutex<Vec<ActorId>>,
}

impl LineWitness {
    fn learn(&self, name: ActorId, successors: &[ActorId]) {
        self.lines.lock().unwrap().insert(name, successors.to_vec());
    }
}

#[async_trait]
impl fauna_conversations::backend::SuccessionWitness for LineWitness {
    async fn verify(
        &self,
        _statement: fauna_core::recovery::SignedIdentitySuccession,
    ) -> Option<fauna_core::recovery::VerifiedSuccession> {
        None
    }

    async fn succession_line(
        &self,
        name: &ActorId,
    ) -> fauna_conversations::backend::SuccessionLine {
        use fauna_conversations::backend::SuccessionLine;
        self.asked.lock().unwrap().push(*name);
        match self.lines.lock().unwrap().get(name) {
            Some(successors) => SuccessionLine::Verified(successors.clone()),
            None => SuccessionLine::NotYet,
        }
    }
}

/// **A succeeded owner's or admin's room keeps working on the member side —
/// through successions the member verified itself, and through nothing else.**
///
/// `conversation-rooms.md` § Roles and authorization → *Delete any message — the
/// mechanism* → *A name designates its verified line*: a policy's name stands
/// for itself and every **verified** successor after it. So, in a room whose
/// owner succeeded twice and whose admin succeeded once:
///
/// - a version signed by the **intermediate** successor (the policy still naming
///   the original owner) extends the chain, and so does the terminal
///   successor's re-sign under its own name;
/// - each successor's floor delete record tombstones its target;
/// - a key on nobody's verified line — a seed thief's — extends nothing and
///   paints nothing, however well-formed its record;
/// - **the refused case**: once a version names the successor itself, the
///   retired key is off that name's line — its record under that version paints
///   nothing;
/// - a seat with **no witness** judges names as themselves: the chain stops at
///   the last version a named key signed (fails closed, as before);
/// - a seat whose witness holds **no anchor yet** paints nothing, and paints on
///   the pass after the anchor lands.
#[tokio::test]
async fn a_succeeded_seats_community_floor_delete_is_honoured_only_through_a_verified_line() {
    use fauna_conversations::room::RoomRole;
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x52u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    // The owner's line: b1 → b2 → b3. The admin's: c1 → c2. And a thief.
    let (owner_1, owner_2, owner_3) = (key(0xb1), key(0xb2), key(0xb3));
    let (admin_1, admin_2, thief) = (key(0xc1), key(0xc2), key(0xee));
    let channel = ChannelId(derive_room_id(&owner_1.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let carol = room_seat(&nest, channel, "carol", 3);
    let bob = room_seat(&nest, channel, "bob", 4);
    let dave = room_seat(&nest, channel, "dave", 5);
    let erin = room_seat(&nest, channel, "erin", 6);
    let gen_key = GenerationKey::mint();
    for seat in [&carol, &bob, &dave, &erin] {
        seat.hold(&gen_key, [0x5eu8; 32]);
    }

    // v1 the founder's; v2 the founder appoints the admin; v3 the INTERMEDIATE
    // successor renames, the policy still naming the founder; v4 the terminal
    // successor re-signs under its own name.
    let v1 = RoomPolicy::initial(owner_1.actor_id(), Some("square".into()))
        .sign_community(&channel.0, &owner_1)
        .unwrap();
    let step = |prev: &RoomPolicy, signer: &ActorKeypair, edit: &dyn Fn(&mut RoomPolicy)| {
        let mut next = prev.clone();
        next.version += 1;
        edit(&mut next);
        next.sign_community(&channel.0, signer).unwrap()
    };
    let v2 = step(&v1.policy, &owner_1, &|p| {
        p.set_admins([admin_1.actor_id()])
    });
    let v3 = step(&v2.policy, &owner_2, &|p| p.name = Some("plaza".into()));
    let v4 = step(&v3.policy, &owner_3, &|p| p.owner = owner_3.actor_id());

    let members = vec![
        RoomRosterKnownMember {
            role: Some(RoomRole::Owner),
            ..known(owner_3.actor_id(), None)
        },
        RoomRosterKnownMember {
            role: Some(RoomRole::Admin),
            ..known(admin_2.actor_id(), None)
        },
        RoomRosterKnownMember {
            kind: RoomPrincipalKind::Nest,
            ..known(ActorId([0x4e; 32]), None)
        },
    ];
    for seat in [&carol, &bob, &dave, &erin] {
        seat.backend
            .set_room_roster_reader(Arc::new(VersionedFloor {
                members: members.clone(),
                versions: vec![v1.clone(), v2.clone(), v3.clone(), v4.clone()],
                salt,
                reachable: Mutex::new(true),
            }));
    }

    // Carol verified both lines (and that the terminal owner never succeeded).
    // Dave registers no witness. Erin's holds no anchor yet.
    let carols = Arc::new(LineWitness::default());
    carols.learn(
        owner_1.actor_id(),
        &[owner_2.actor_id(), owner_3.actor_id()],
    );
    carols.learn(admin_1.actor_id(), &[admin_2.actor_id()]);
    carols.learn(owner_3.actor_id(), &[]);
    carol.backend.set_succession_witness(carols.clone());
    let erins = Arc::new(LineWitness::default());
    erin.backend.set_succession_witness(erins.clone());

    for text in ["one", "two", "three", "four", "five"] {
        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: text.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
    }
    // seq 6 — the terminal owner under v4, by name. seq 7 — the intermediate
    // owner under v3, by line. seq 8 — the admin's successor under v3, by line.
    // seq 9 — the thief under v4. seq 10 — the RETIRED founder key under v4,
    // which names its successor.
    for (signer, target, version) in [
        (0xb3u8, 1, 4),
        (0xb2, 2, 3),
        (0xc2, 3, 3),
        (0xee, 4, 4),
        (0xb1, 5, 4),
    ] {
        let engine = MlsEngine::new_in_memory(key(signer)).unwrap();
        nest.push_envelope(
            &hex,
            ChannelEnvelope::RoomFloorDelete(
                engine
                    .sign_room_floor_delete(&channel.0, target, version)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .to_bytes()
            .unwrap(),
        );
    }

    let mut cursors = [0i64; 3];
    let seats = [&carol, &dave, &erin];
    for (seat, cursor) in seats.iter().zip(cursors.iter_mut()) {
        seat.poll(&channel, cursor).await;
    }
    let painted = |seat: &RoomSeat| {
        let detail = seat
            .manager
            .thread_detail(seat.thread.clone())
            .expect("thread");
        (1..=5u64)
            .filter(|seq| {
                detail
                    .messages
                    .iter()
                    .find(|m| m.message_id == MessageId(format!("conv:{hex}:{seq}")))
                    .expect("the bubble is held")
                    .deleted
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(
        painted(&carol),
        vec![1, 2, 3],
        "each successor's record tombstones its target through the line carol \
         verified — the intermediate owner's version 3 anchored on the way; the \
         thief is on nobody's line, and the retired founder key is off the line \
         of a version that names its successor"
    );
    assert_eq!(
        painted(&dave),
        Vec::<u64>::new(),
        "no witness: names are judged as themselves, the chain stops at version \
         2, and no record naming a later version paints"
    );
    assert_eq!(
        painted(&erin),
        Vec::<u64>::new(),
        "no anchor yet: an unverified succession grants nothing"
    );

    // Erin's harvest seeds the anchors; the parked records paint on the next
    // pass.
    erins.learn(
        owner_1.actor_id(),
        &[owner_2.actor_id(), owner_3.actor_id()],
    );
    erins.learn(admin_1.actor_id(), &[admin_2.actor_id()]);
    erins.learn(owner_3.actor_id(), &[]);
    erin.poll(&channel, &mut cursors[2]).await;
    assert_eq!(
        painted(&erin),
        vec![1, 2, 3],
        "a record parked for want of a verified line is judged again, and \
         painted, on the pass after the line verifies"
    );
    assert!(
        !carols.asked.lock().unwrap().contains(&thief.actor_id()),
        "only a POLICY's names are ever asked about — a record's author never \
         chooses whom this device dials"
    );
}

/// **A name this device verified as never-succeeded is asked again — bounded —
/// so a LATER succession of it is honoured in the same session.**
///
/// `conversation-rooms.md` § Roles and authorization → *A name designates its
/// verified line*: an empty line is "no successor *so far*". Settled for good,
/// it made every floor delete a later successor signed a member's claim — a
/// refusal no parked-record retry or relaunch ever revisits, since the walk had
/// already stepped past the record. So, in a room whose admin succeeds AFTER a
/// refused record settled both of the policy's names as empty:
///
/// - a rank refusal while a policy name's line is verified empty **parks** — it
///   never becomes a member's claim (the thief's record parks, bounded by the
///   park cap, and never paints);
/// - the empty line is asked again once every
///   [`LINE_REASK_PASSES`](fauna_conversations::backends::fauna_mls::LINE_REASK_PASSES)
///   passes, not on every pass — and the successor's parked record paints on
///   the pass the re-ask learns the line.
#[tokio::test]
async fn a_later_succession_of_a_name_settled_empty_is_honoured_in_the_same_session() {
    use fauna_conversations::backends::fauna_mls::LINE_REASK_PASSES;
    use fauna_conversations::room::RoomRole;
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x54u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (owner, admin_1, admin_2, thief) = (key(0xb1), key(0xc1), key(0xc2), key(0xee));
    let channel = ChannelId(derive_room_id(&owner.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let carol = room_seat(&nest, channel, "carol", 3);
    let bob = room_seat(&nest, channel, "bob", 4);
    let gen_key = GenerationKey::mint();
    for seat in [&carol, &bob] {
        seat.hold(&gen_key, [0x5eu8; 32]);
    }

    let v1 = RoomPolicy::initial(owner.actor_id(), Some("square".into()))
        .sign_community(&channel.0, &owner)
        .unwrap();
    let mut p2 = v1.policy.clone();
    p2.version = 2;
    p2.set_admins([admin_1.actor_id()]);
    let v2 = p2.sign_community(&channel.0, &owner).unwrap();
    carol
        .backend
        .set_room_roster_reader(Arc::new(VersionedFloor {
            members: vec![
                RoomRosterKnownMember {
                    role: Some(RoomRole::Owner),
                    ..known(owner.actor_id(), None)
                },
                RoomRosterKnownMember {
                    role: Some(RoomRole::Admin),
                    ..known(admin_2.actor_id(), None)
                },
                RoomRosterKnownMember {
                    kind: RoomPrincipalKind::Nest,
                    ..known(ActorId([0x4e; 32]), None)
                },
            ],
            versions: vec![v1, v2],
            salt,
            reachable: Mutex::new(true),
        }));
    // Carol verified that neither name ever succeeded — true, when she asked.
    let carols = Arc::new(LineWitness::default());
    carols.learn(owner.actor_id(), &[]);
    carols.learn(admin_1.actor_id(), &[]);
    carol.backend.set_succession_witness(carols.clone());

    for text in ["one", "two", "three"] {
        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: text.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
    }
    let push_floor_delete = |signer: u8, target: u64| {
        let engine = MlsEngine::new_in_memory(key(signer)).unwrap();
        nest.push_envelope(
            &hex,
            ChannelEnvelope::RoomFloorDelete(
                engine
                    .sign_room_floor_delete(&channel.0, target, 2)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .to_bytes()
            .unwrap(),
        );
    };
    let painted = || {
        let detail = carol
            .manager
            .thread_detail(carol.thread.clone())
            .expect("thread");
        (1..=3u64)
            .filter(|seq| {
                detail
                    .messages
                    .iter()
                    .find(|m| m.message_id == MessageId(format!("conv:{hex}:{seq}")))
                    .expect("the bubble is held")
                    .deleted
            })
            .collect::<Vec<_>>()
    };
    let asks = |name: ActorId| {
        carols
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|asked| **asked == name)
            .count()
    };

    // seq 4 — the thief's record: a rank refusal that settles both names empty.
    push_floor_delete(0xee, 1);
    let mut cursor = 0i64;
    carol.poll(&channel, &mut cursor).await;
    assert_eq!(painted(), Vec::<u64>::new());
    assert_eq!((asks(owner.actor_id()), asks(admin_1.actor_id())), (1, 1));

    // The admin succeeds only now; seq 5 is the successor's record.
    carols.learn(admin_1.actor_id(), &[admin_2.actor_id()]);
    push_floor_delete(0xc2, 2);
    carol.poll(&channel, &mut cursor).await;
    assert_eq!(
        carol.manager.parked_floor_deletes(&channel).len(),
        2,
        "under a line verified empty a rank refusal parks — neither record is \
         recorded as a member's claim the walk would never meet again"
    );

    for _ in 0..LINE_REASK_PASSES {
        carol.poll(&channel, &mut cursor).await;
    }
    assert_eq!(
        painted(),
        vec![2],
        "the empty line is asked again and the successor's parked record paints \
         in the SAME session; the thief's never does"
    );
    assert_eq!(
        (asks(owner.actor_id()), asks(admin_1.actor_id())),
        (2, 2),
        "bounded: over {} passes each empty line was asked once more, not once \
         per pass",
        LINE_REASK_PASSES + 2
    );
    assert_eq!(
        carol.manager.parked_floor_deletes(&channel).len(),
        1,
        "the thief's record stays parked — bounded by the park cap, never painted"
    );
    assert!(
        !carols.asked.lock().unwrap().contains(&thief.actor_id()),
        "a record's author never chooses whom this device dials"
    );
}

/// **A name's verified POSITIVE line is asked again — bounded — so a successor
/// that itself succeeds later is honoured in the same session.**
///
/// `conversation-rooms.md` § Roles and authorization → *A name designates its
/// verified line* → *A verified line holds only so far*: the empty line's
/// positive twin. A member that verified A1 → A2 and kept that line for the
/// session judged A3 — A2's own later successor — a plain member for good on
/// that device: A3's record was recorded as a member's claim, a verdict no
/// parked-record retry or relaunch ever revisits. So, in a room whose owner's
/// verified successor succeeds in turn AFTER the member honoured the
/// successor's own record:
///
/// - the third holder's record **parks** — it never becomes a member's claim;
/// - the positive line is asked again once every
///   [`LINE_REASK_PASSES`](fauna_conversations::backends::fauna_mls::LINE_REASK_PASSES)
///   passes, not on every pass — and the record paints on the pass the re-ask
///   learns the longer line;
/// - a thief's record parks alongside it, bounded by the park cap, and never
///   paints — with a witness registered no rank refusal settles as a member's
///   claim, since every line may yet grow.
#[tokio::test]
async fn a_successors_own_later_succession_is_honoured_in_the_same_session() {
    use fauna_conversations::backends::fauna_mls::LINE_REASK_PASSES;
    use fauna_conversations::room::RoomRole;
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x55u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (owner_1, owner_2, owner_3, thief) = (key(0xb1), key(0xb2), key(0xb3), key(0xee));
    let channel = ChannelId(derive_room_id(&owner_1.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let carol = room_seat(&nest, channel, "carol", 3);
    let bob = room_seat(&nest, channel, "bob", 4);
    let gen_key = GenerationKey::mint();
    for seat in [&carol, &bob] {
        seat.hold(&gen_key, [0x5fu8; 32]);
    }

    let v1 = RoomPolicy::initial(owner_1.actor_id(), Some("square".into()))
        .sign_community(&channel.0, &owner_1)
        .unwrap();
    carol
        .backend
        .set_room_roster_reader(Arc::new(VersionedFloor {
            members: vec![
                RoomRosterKnownMember {
                    role: Some(RoomRole::Owner),
                    ..known(owner_3.actor_id(), None)
                },
                RoomRosterKnownMember {
                    kind: RoomPrincipalKind::Nest,
                    ..known(ActorId([0x4e; 32]), None)
                },
            ],
            versions: vec![v1],
            salt,
            reachable: Mutex::new(true),
        }));
    // Carol verified the owner's line as far as it went: one successor.
    let carols = Arc::new(LineWitness::default());
    carols.learn(owner_1.actor_id(), &[owner_2.actor_id()]);
    carol.backend.set_succession_witness(carols.clone());

    for text in ["one", "two", "three"] {
        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: text.into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
    }
    let push_floor_delete = |signer: u8, target: u64| {
        let engine = MlsEngine::new_in_memory(key(signer)).unwrap();
        nest.push_envelope(
            &hex,
            ChannelEnvelope::RoomFloorDelete(
                engine
                    .sign_room_floor_delete(&channel.0, target, 1)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .to_bytes()
            .unwrap(),
        );
    };
    let painted = || {
        let detail = carol
            .manager
            .thread_detail(carol.thread.clone())
            .expect("thread");
        (1..=3u64)
            .filter(|seq| {
                detail
                    .messages
                    .iter()
                    .find(|m| m.message_id == MessageId(format!("conv:{hex}:{seq}")))
                    .expect("the bubble is held")
                    .deleted
            })
            .collect::<Vec<_>>()
    };
    let asks = |name: ActorId| {
        carols
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|asked| **asked == name)
            .count()
    };

    // seq 4 — the verified successor's record: honoured through the line.
    push_floor_delete(0xb2, 1);
    let mut cursor = 0i64;
    carol.poll(&channel, &mut cursor).await;
    assert_eq!(painted(), vec![1]);
    assert_eq!(asks(owner_1.actor_id()), 1);

    // The successor succeeds in turn only now; seq 5 is the third holder's
    // record, seq 6 a thief's.
    carols.learn(
        owner_1.actor_id(),
        &[owner_2.actor_id(), owner_3.actor_id()],
    );
    push_floor_delete(0xb3, 2);
    push_floor_delete(0xee, 3);
    carol.poll(&channel, &mut cursor).await;
    assert_eq!(painted(), vec![1]);
    assert_eq!(
        carol.manager.parked_floor_deletes(&channel).len(),
        2,
        "under a line held positive a rank refusal still parks — neither record \
         is recorded as a member's claim the walk would never meet again"
    );

    for _ in 0..LINE_REASK_PASSES {
        carol.poll(&channel, &mut cursor).await;
    }
    assert_eq!(
        painted(),
        vec![1, 2],
        "the positive line is asked again and the third holder's parked record \
         paints in the SAME session; the thief's never does"
    );
    assert_eq!(
        asks(owner_1.actor_id()),
        2,
        "bounded: over {} passes the positive line was asked once more, not \
         once per pass",
        LINE_REASK_PASSES + 2
    );
    assert_eq!(
        carol.manager.parked_floor_deletes(&channel).len(),
        1,
        "the thief's record stays parked — bounded by the park cap, never painted"
    );
    assert!(
        !carols.asked.lock().unwrap().contains(&thief.actor_id()),
        "a record's author never chooses whom this device dials"
    );
}

/// **A policy name this member never met is offered to the peer-anchor harvest
/// — from the ANCHORED chain, and from nowhere else.**
///
/// `identity-succession.md` § The succession statement → *a community policy's
/// names join the harvest's walk*: a member who joined after the owner
/// succeeded shares no thread with the retired identity, so the sweep's thread
/// walk never reaches it and the line never resolves. The room therefore
/// offers the sweep the names a rank refusal asked about and found no anchor
/// for — but only names a version this device **anchored** carries. The
/// version still waiting on the refusal is bytes the room's home nest served
/// and nobody on a verified line signed: a name only *it* carries is the
/// home nest's choice of whom this device fetches, and is never offered. The
/// offer is bounded per room, because a policy's admin set is not.
#[tokio::test]
async fn a_never_met_policy_name_is_offered_to_the_harvest_from_the_anchored_chain_only() {
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x53u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (founder, successor, served_only) = (key(0xa1), key(0xa2), key(0xd1));
    let admins: Vec<ActorId> = (0x10u8..0x1c).map(|seed| key(seed).actor_id()).collect();
    let channel = ChannelId(derive_room_id(&founder.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let bob = room_seat(&nest, channel, "bob", 4);
    let erin = room_seat(&nest, channel, "erin", 6);
    let gen_key = GenerationKey::mint();
    for seat in [&bob, &erin] {
        seat.hold(&gen_key, [0x5eu8; 32]);
    }

    // v1 the founder's; v2 the founder appoints twelve admins; v3 the
    // successor's re-sign under its own name, which also appoints one more.
    let v1 = RoomPolicy::initial(founder.actor_id(), Some("square".into()))
        .sign_community(&channel.0, &founder)
        .unwrap();
    let mut p2 = v1.policy.clone();
    p2.version = 2;
    p2.set_admins(admins.clone());
    let v2 = p2.sign_community(&channel.0, &founder).unwrap();
    let mut p3 = v2.policy.clone();
    p3.version = 3;
    p3.owner = successor.actor_id();
    p3.set_admins(admins.iter().copied().chain([served_only.actor_id()]));
    let v3 = p3.sign_community(&channel.0, &successor).unwrap();

    erin.backend
        .set_room_roster_reader(Arc::new(VersionedFloor {
            members: vec![RoomRosterKnownMember {
                role: Some(fauna_conversations::room::RoomRole::Owner),
                ..known(successor.actor_id(), None)
            }],
            versions: vec![v1, v2.clone(), v3],
            salt,
            reachable: Mutex::new(true),
        }));
    let erins = Arc::new(LineWitness::default());
    erin.backend.set_succession_witness(erins.clone());

    bob.backend
        .send(
            &fauna_mls_thread(bob.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "one".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("a keyed seat can send into its room");
    let engine = MlsEngine::new_in_memory(key(0xa2)).unwrap();
    nest.push_envelope(
        &hex,
        ChannelEnvelope::RoomFloorDelete(
            engine
                .sign_room_floor_delete(&channel.0, 1, 3)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .to_bytes()
        .unwrap(),
    );

    // Enough passes for the per-judgment ask budget to reach every name both
    // policies carry — the served-only ones included.
    let mut cursor = 0i64;
    for _ in 0..6 {
        erin.poll(&channel, &mut cursor).await;
    }
    let asked = erins.asked.lock().unwrap().clone();
    assert!(
        asked.contains(&served_only.actor_id()) && asked.contains(&successor.actor_id()),
        "the control: the refusal did ask about the names only the unanchored \
         version carries, so their absence below is the filter's doing"
    );
    let offered = erin.manager.policy_anchor_wants();
    assert_eq!(
        offered.first(),
        Some(&founder.actor_id()),
        "the retired founder — a name this member shares no thread with — is \
         offered to the harvest, owner first"
    );
    assert!(
        !offered.contains(&served_only.actor_id()) && !offered.contains(&successor.actor_id()),
        "a name only the served, still-unanchored version carries is never \
         offered: the room's home nest does not choose whom this device fetches"
    );
    assert!(
        offered
            .iter()
            .all(|name| *name == founder.actor_id() || v2.policy.admins.contains(name)),
        "every offered name is one an anchored version carries"
    );
    assert_eq!(
        offered.len(),
        8,
        "the offer is bounded per room — a policy's admin set is not"
    );

    // The harvest seeds the founder's anchor and the line verifies: the name
    // is settled, so it leaves the offer, and the parked record paints.
    erins.learn(founder.actor_id(), &[successor.actor_id()]);
    erin.poll(&channel, &mut cursor).await;
    assert!(
        !erin
            .manager
            .policy_anchor_wants()
            .contains(&founder.actor_id()),
        "a name whose line verified is no longer wanted"
    );
    // The bound is a FIXED PREFIX of the anchored chain, never a rolling
    // window. A settled name leaving the offer must not pull the ninth name
    // in behind it: every settle would then admit another, and a founder
    // naming K same-nest identities would seed K entries into an anchor store
    // that is bounded for the whole account and never evicts — not the eight
    // the ruling grants a room.
    // Chain order: the policy's own (canonical) admin order, not the order
    // the fixture happened to mint them in.
    let first_eight: Vec<ActorId> = std::iter::once(founder.actor_id())
        .chain(v2.policy.admins.iter().copied())
        .take(8)
        .collect();
    let after_settle = erin.manager.policy_anchor_wants();
    assert_eq!(
        after_settle.len(),
        7,
        "the settled founder leaves; nothing takes its place"
    );
    assert!(
        after_settle.iter().all(|name| first_eight.contains(name)),
        "a room only ever offers the first eight names its anchored chain \
         carries, however many of them have settled"
    );
    let detail = erin
        .manager
        .thread_detail(erin.thread.clone())
        .expect("thread");
    assert!(
        detail
            .messages
            .iter()
            .find(|m| m.message_id == MessageId(format!("conv:{hex}:1")))
            .expect("the bubble is held")
            .deleted,
        "the successor's record paints once the never-met founder's line verifies"
    );
}

/// **A room whose policy chain this device cannot prove says so — once the
/// harvest has spoken, and never as a mark on the record's target.**
///
/// `conversation-rooms.md` § Roles and authorization → *Delete any message —
/// the mechanism* → *Members verify what they paint*: a floor delete record
/// parked on a name this device holds no anchor for paints nothing, and a
/// retired name homed on another nest never anchors — the harvest's reach is
/// same-nest only, permanently (`identity-succession.md` § The succession
/// statement → *a community policy's names join the harvest's walk*, bound 3).
/// Silence would show the target as if nobody had acted on it, so the room
/// reports its moderation as unverified (`RoomSnapshot::moderation_unverified`)
/// — but only once the peer-anchor harvest has **settled** the name, since
/// before that the record is merely *not yet* judged and the next pass may
/// paint it. The target stays painted throughout, and the report clears the
/// moment a verified line lets the record paint.
#[tokio::test]
async fn a_room_says_so_once_its_parked_moderation_rests_on_a_name_the_harvest_settled_without_an_anchor()
 {
    use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

    let nest = Arc::new(MockNest::default());
    let salt = fauna_mls::room_policy::binding_birth_salt(&[0x54u8; 24]);
    let key = |seed: u8| ActorKeypair::from_secret([seed; 32]);
    let (founder, successor, stranger) = (key(0xb1), key(0xb2), key(0xb3));
    let channel = ChannelId(derive_room_id(&founder.actor_id(), &salt).unwrap());
    let hex = channel.to_string();

    let bob = room_seat(&nest, channel, "bob", 4);
    let erin = room_seat(&nest, channel, "erin", 6);
    let gen_key = GenerationKey::mint();
    for seat in [&bob, &erin] {
        seat.hold(&gen_key, [0x5fu8; 32]);
    }

    // v1 the founder's; v2 the successor's re-sign under its own name — a step
    // only the founder's verified line can carry.
    let v1 = RoomPolicy::initial(founder.actor_id(), Some("square".into()))
        .sign_community(&channel.0, &founder)
        .unwrap();
    let mut p2 = v1.policy.clone();
    p2.version = 2;
    p2.owner = successor.actor_id();
    let v2 = p2.sign_community(&channel.0, &successor).unwrap();

    erin.backend
        .set_room_roster_reader(Arc::new(VersionedFloor {
            members: vec![RoomRosterKnownMember {
                role: Some(fauna_conversations::room::RoomRole::Owner),
                ..known(successor.actor_id(), None)
            }],
            versions: vec![v1, v2],
            salt,
            reachable: Mutex::new(true),
        }));
    let erins = Arc::new(LineWitness::default());
    erin.backend.set_succession_witness(erins.clone());

    bob.backend
        .send(
            &fauna_mls_thread(bob.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "one".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("a keyed seat can send into its room");
    let engine = MlsEngine::new_in_memory(key(0xb2)).unwrap();
    nest.push_envelope(
        &hex,
        ChannelEnvelope::RoomFloorDelete(
            engine
                .sign_room_floor_delete(&channel.0, 1, 2)
                .unwrap()
                .to_bytes()
                .unwrap(),
        )
        .to_bytes()
        .unwrap(),
    );

    let room = |seat: &RoomSeat| {
        let detail = seat
            .manager
            .thread_detail(seat.thread.clone())
            .expect("thread");
        let deleted = detail
            .messages
            .iter()
            .find(|m| m.message_id == MessageId(format!("conv:{hex}:1")))
            .expect("the bubble is held")
            .deleted;
        (detail.room.expect("a room").moderation_unverified, deleted)
    };

    let mut cursor = 0i64;
    erin.poll(&channel, &mut cursor).await;
    assert!(
        erins.asked.lock().unwrap().contains(&founder.actor_id()),
        "the control: the refusal asked about the founder, so the record is parked on \
         that name"
    );
    assert_eq!(
        room(&erin),
        (false, false),
        "parked on a name the harvest has not reached yet: not yet judged, and the \
         room says nothing — the next pass may still paint it"
    );

    // The harvest settles a peer the room never asked about: nothing turns.
    settle_parked_successions(&erin.backend, &erin.manager, &stranger.actor_id()).await;
    assert_eq!(
        room(&erin),
        (false, false),
        "a settle for a name no judgment waits on says nothing about this room"
    );

    // The harvest settles the founder — a retired name homed on another nest,
    // say — with nothing to show: the record is not going to paint this
    // session, and the room says so. Its target stays painted.
    settle_parked_successions(&erin.backend, &erin.manager, &founder.actor_id()).await;
    assert_eq!(
        room(&erin),
        (true, false),
        "the parked record rests on a name the harvest settled without an anchor: the \
         room reports its moderation as unverified, and the target is still shown"
    );
    erin.poll(&channel, &mut cursor).await;
    assert_eq!(
        room(&erin),
        (true, false),
        "the retry parks it again on the same name — the report stands across passes"
    );

    // A line verifies after all (a same-nest name the harvest did seed): the
    // record paints, and there is nothing left to say so about.
    erins.learn(founder.actor_id(), &[successor.actor_id()]);
    erin.poll(&channel, &mut cursor).await;
    assert_eq!(
        room(&erin),
        (false, true),
        "once the line verifies the record paints, and the report clears with it"
    );
}

/// The room of the pin above, reduced to what the set and clear rules' own
/// pins need: version 1 the founder's, a served version 2 re-signed by
/// `v2_signer` under its own name, and that key's floor delete record naming
/// version 2 on the log — parked after erin's first poll, because only the
/// founder's line could carry the step and nobody has verified it.
struct ParkedModerationRoom {
    erin: RoomSeat,
    witness: Arc<LineWitness>,
    channel: ChannelId,
    founder: ActorId,
    cursor: i64,
    nest: Arc<MockNest>,
    floor: Arc<VersionedFloor>,
    /// `v2_signer`'s engine, to sign a later floor delete record.
    v2_engine: MlsEngine,
}

impl ParkedModerationRoom {
    async fn parked(v2_signer: ActorKeypair) -> Self {
        use fauna_mls::room_policy::{RoomPolicy, derive_room_id};

        let nest = Arc::new(MockNest::default());
        let salt = fauna_mls::room_policy::binding_birth_salt(&[0x56u8; 24]);
        let founder = ActorKeypair::from_secret([0xc1; 32]);
        let channel = ChannelId(derive_room_id(&founder.actor_id(), &salt).unwrap());

        let bob = room_seat(&nest, channel, "bob", 4);
        let erin = room_seat(&nest, channel, "erin", 6);
        let gen_key = GenerationKey::mint();
        for seat in [&bob, &erin] {
            seat.hold(&gen_key, [0x5eu8; 32]);
        }
        let v1 = RoomPolicy::initial(founder.actor_id(), Some("square".into()))
            .sign_community(&channel.0, &founder)
            .unwrap();
        let mut p2 = v1.policy.clone();
        p2.version = 2;
        p2.owner = v2_signer.actor_id();
        let v2 = p2.sign_community(&channel.0, &v2_signer).unwrap();
        let floor = Arc::new(VersionedFloor {
            members: vec![RoomRosterKnownMember {
                role: Some(fauna_conversations::room::RoomRole::Owner),
                ..known(v2_signer.actor_id(), None)
            }],
            versions: vec![v1, v2],
            salt,
            reachable: Mutex::new(true),
        });
        erin.backend.set_room_roster_reader(floor.clone());
        let witness = Arc::new(LineWitness::default());
        erin.backend.set_succession_witness(witness.clone());

        bob.backend
            .send(
                &fauna_mls_thread(bob.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: "one".into(),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("a keyed seat can send into its room");
        let mut room = Self {
            erin,
            witness,
            channel,
            founder: founder.actor_id(),
            cursor: 0,
            nest,
            floor,
            v2_engine: MlsEngine::new_in_memory(v2_signer).unwrap(),
        };
        room.push_floor_delete(2);
        room.poll().await;
        assert!(
            room.witness.asked.lock().unwrap().contains(&room.founder),
            "the control: the refusal asked about the founder, so the record is parked"
        );
        assert_eq!(
            room.state(),
            (false, false),
            "the control: nothing said yet"
        );
        room
    }

    async fn poll(&mut self) {
        self.erin.poll(&self.channel, &mut self.cursor).await;
    }

    /// Put `v2_signer`'s floor delete record for message 1, naming policy
    /// `version`, on the room log.
    fn push_floor_delete(&self, version: u64) {
        self.nest.push_envelope(
            &self.channel.to_string(),
            ChannelEnvelope::RoomFloorDelete(
                self.v2_engine
                    .sign_room_floor_delete(&self.channel.0, 1, version)
                    .unwrap()
                    .to_bytes()
                    .unwrap(),
            )
            .to_bytes()
            .unwrap(),
        );
    }

    /// How many floor delete records the room holds parked.
    fn parked_count(&self) -> usize {
        self.erin
            .manager
            .snapshot_channel_slice(&self.erin.thread, &self.channel.to_string(), self.cursor)
            .expect("snapshots")
            .parked_floor_deletes
            .len()
    }

    /// `(moderation_unverified, the target painted deleted)`.
    fn state(&self) -> (bool, bool) {
        let detail = self
            .erin
            .manager
            .thread_detail(self.erin.thread.clone())
            .expect("thread");
        let deleted = detail
            .messages
            .iter()
            .find(|m| m.message_id == MessageId(format!("conv:{}:1", self.channel)))
            .expect("the bubble is held")
            .deleted;
        (detail.room.expect("a room").moderation_unverified, deleted)
    }
}

/// **A home nest cannot make a room say its moderation is unverified.** The
/// served version 2 is bytes nobody on a verified line signed, so the names it
/// carries are not the room's: a record parked beside it waits on them too,
/// but the harvest settling one of them — a roster member the sweep walks
/// anyway — is no statement about this room (`conversation-rooms.md` § Roles
/// and authorization → *A room whose chain this device cannot prove says so*:
/// the names are the ones "the peer-anchor harvest has settled … without
/// anchoring" among a chain this device proved). Only the anchored chain's
/// own name, settled, says so.
#[tokio::test]
async fn a_name_only_the_served_unanchored_version_carries_never_says_a_room_is_unverified() {
    let impostor = ActorKeypair::from_secret([0xc3; 32]).actor_id();
    let mut room = ParkedModerationRoom::parked(ActorKeypair::from_secret([0xc3; 32])).await;
    assert!(
        room.witness.asked.lock().unwrap().contains(&impostor),
        "the control: the refusal asked about the served version's own name too"
    );

    // The harvest's roster walk settles the served version's name with nothing
    // to show.
    settle_parked_successions(&room.erin.backend, &room.erin.manager, &impostor).await;
    assert_eq!(
        room.state(),
        (false, false),
        "a name only the home nest's served, unanchored version carries is not the \
         room's: its settle says nothing about the room's moderation"
    );
    room.poll().await;
    assert_eq!(
        room.state(),
        (false, false),
        "nor after the retry re-parks it"
    );

    // The control: the anchored chain's own name, settled, does.
    let founder = room.founder;
    settle_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(
        room.state(),
        (true, false),
        "the founder's name is the anchored chain's: settled without an anchor, the \
         room says so"
    );
}

/// **A seed is an anchor, not a settle-without-one: the room never announces
/// moderation it is about to verify.** The harvest seeding the very name a
/// parked record waits on is the path by which that record paints on the next
/// pass, so between the seed and that pass the room says nothing — the
/// statement is for a name "settled … without anchoring" (`conversation-rooms.md`
/// § Roles and authorization → *A room whose chain this device cannot prove
/// says so*).
#[tokio::test]
async fn a_seeded_name_never_announces_the_room_as_unverified_before_the_record_paints() {
    let mut room = ParkedModerationRoom::parked(ActorKeypair::from_secret([0xc2; 32])).await;
    let successor = ActorKeypair::from_secret([0xc2; 32]).actor_id();
    let founder = room.founder;

    // The harvest seeds the founder's anchor and the line verifies.
    room.witness.learn(founder, &[successor]);
    redrive_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(
        room.state(),
        (false, false),
        "a seed is an anchor: the room says nothing before the next pass judges again"
    );
    room.poll().await;
    assert_eq!(
        room.state(),
        (false, true),
        "the next pass paints the record under the verified line"
    );
}

/// **A seed after a settle takes the statement back.** The harvest may settle
/// a name with nothing to show and seed it on a later read in the same
/// session; from the seed on the name is anchored, not "settled … without
/// anchoring" (`conversation-rooms.md` § Roles and authorization → *A room
/// whose chain this device cannot prove says so*), so the room stops saying
/// so at the seed — not only at the pass that paints.
#[tokio::test]
async fn a_seed_after_a_settle_withdraws_the_rooms_unverified_statement_before_the_record_paints() {
    let mut room = ParkedModerationRoom::parked(ActorKeypair::from_secret([0xc4; 32])).await;
    let successor = ActorKeypair::from_secret([0xc4; 32]).actor_id();
    let founder = room.founder;

    settle_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(
        room.state(),
        (true, false),
        "the control: settled without an anchor, the room says so"
    );

    room.witness.learn(founder, &[successor]);
    redrive_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(
        room.state(),
        (false, false),
        "the seed anchors the name: the statement goes before the next pass judges"
    );
    room.poll().await;
    assert_eq!(
        room.state(),
        (false, true),
        "and the next pass paints the record"
    );
}

/// **A record parked for want of a version is *not yet*, whatever an earlier
/// record's names did.** The rule's name is one "**the record itself** still
/// waits on" (`conversation-rooms.md` § Roles and authorization → *A room
/// whose chain this device cannot prove says so*): a name an earlier judgment
/// asked about — still unestablished, on the anchored chain, and settled by
/// the harvest — says nothing about a later record whose only wait is a
/// version the home nest cannot serve this pass.
#[tokio::test]
async fn a_record_parked_for_want_of_a_version_is_not_yet_whatever_an_earlier_records_names_did() {
    let mut room = ParkedModerationRoom::parked(ActorKeypair::from_secret([0xc5; 32])).await;
    let successor = ActorKeypair::from_secret([0xc5; 32]).actor_id();
    let founder = room.founder;
    assert!(
        room.witness.asked.lock().unwrap().contains(&successor),
        "the control: the first record's judgment asked about version 2's own name, \
         which nothing establishes"
    );

    // The harvest settles version 2's name — not yet the anchored chain's.
    settle_parked_successions(&room.erin.backend, &room.erin.manager, &successor).await;
    assert_eq!(room.state(), (false, false));

    // The founder's line verifies: the first record paints and the chain
    // anchors version 2, so the settled name is the anchored chain's now.
    room.witness.learn(founder, &[successor]);
    room.poll().await;
    assert_eq!(room.state(), (false, true), "the first record paints");
    assert_eq!(room.parked_count(), 0, "and leaves the parked set");

    // A second record names version 3, which the home nest cannot serve this
    // pass: parked for want of a version, and waiting on no name of its own.
    *room.floor.reachable.lock().unwrap() = false;
    room.push_floor_delete(3);
    room.poll().await;
    assert_eq!(
        room.parked_count(),
        1,
        "the control: the second record is parked"
    );
    assert_eq!(
        room.state(),
        (false, true),
        "parked for want of a version: not yet judged, and the room says nothing"
    );
}

/// **The statement repaints on a turn, and only then.** It is a fact no ingest
/// carries, so the backend announces it itself — when it comes and when it
/// goes; a settle for a name no record waits on, a second settle of the same
/// name, and a retry pass that re-parks the record where it stood change
/// nothing, and say nothing.
#[tokio::test]
async fn the_unverified_statement_repaints_on_a_turn_and_only_then() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Ticks(AtomicUsize);
    impl SnapshotObserver for Ticks {
        fn on_changed(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let mut room = ParkedModerationRoom::parked(ActorKeypair::from_secret([0xc6; 32])).await;
    let founder = room.founder;
    let stranger = ActorKeypair::from_secret([0xc7; 32]).actor_id();
    let ticks = Arc::new(Ticks(AtomicUsize::new(0)));
    room.erin.manager.add_observer(ticks.clone());
    let take = || ticks.0.swap(0, Ordering::SeqCst);

    room.poll().await;
    assert_eq!(room.state(), (false, false));
    assert_eq!(take(), 0, "a re-park that stays silent says nothing");

    settle_parked_successions(&room.erin.backend, &room.erin.manager, &stranger).await;
    assert_eq!(
        take(),
        0,
        "a settle for a name no record waits on says nothing"
    );

    settle_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(room.state(), (true, false));
    assert!(take() > 0, "the statement coming is news");

    settle_parked_successions(&room.erin.backend, &room.erin.manager, &founder).await;
    assert_eq!(take(), 0, "the same settle again turns nothing");

    room.poll().await;
    assert_eq!(room.state(), (true, false));
    assert_eq!(
        take(),
        0,
        "a retry pass that re-parks where it stood says nothing"
    );
}

/// **What the room's home nest labelled reaches the bubble — merged into the
/// device's own verdicts, not substituted for them.**
///
/// A community room's home nest runs the labelers the room names and serves
/// their verdicts beside the envelope (`conversation-rooms.md` § The three
/// classes → *What the home nest does with its read*, purpose 2). The reading
/// device still classifies what it opened, as on every class; the two meet by
/// the moderation queue's superset rule (`moderation.md` § Implementation
/// status today → *Web queue data-source*): a server verdict wins its
/// category, one the server did not make stays — and never left the device.
#[tokio::test]
async fn a_community_messages_server_labels_merge_into_the_bubbles_own() {
    use fauna_core::content_category::ContentLabelEntry;
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2bu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5bu8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    // The device's own heuristic reads this as phishing AND spam.
    let body = "URGENT: verify your account password - click here, buy now";
    let local: Vec<ContentLabelEntry> = fauna_core::text_heuristic::classify_text(body)
        .into_iter()
        .map(|r| ContentLabelEntry {
            category: r.category,
            confidence_per_mille: (r.confidence * 1000.0).round() as u16,
        })
        .collect();
    let local_categories: Vec<&str> = local.iter().map(|l| l.category.as_str()).collect();
    assert!(
        local_categories.contains(&"phishing") && local_categories.contains(&"spam"),
        "the fixture needs both local categories to prove anything: {local:?}"
    );

    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: body.into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("a keyed seat can send into its room");

    // The room's labelers disagree with the device on phishing and add a
    // category the device's heuristic never produces.
    let server = vec![
        ContentLabelEntry {
            category: "phishing".into(),
            confidence_per_mille: 300,
        },
        ContentLabelEntry {
            category: "nsfw".into(),
            confidence_per_mille: 700,
        },
    ];
    nest.serve_labels(&channel.to_string(), 1, server.clone());

    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let detail = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread");
    let bubble = detail
        .messages
        .iter()
        .find(|m| m.body == body)
        .expect("the message opened into bob's thread");

    assert_eq!(
        bubble.labels,
        fauna_core::content_category::merge_server_labels(&server, &local),
    );
    let confidence = |category: &str| {
        bubble
            .labels
            .iter()
            .find(|l| l.category == category)
            .map(|l| l.confidence_per_mille)
    };
    assert_eq!(
        confidence("phishing"),
        Some(300),
        "the server's verdict wins its category, even over a stronger local one"
    );
    assert_eq!(
        confidence("nsfw"),
        Some(700),
        "a category only the nest saw is kept"
    );
    assert!(
        confidence("spam").is_some(),
        "a category only the device saw is kept"
    );
}

/// **A picture in a community room.** The attachment seals under the room's
/// **attachment** content kind off the message's generation, rests as one
/// content-addressed blob the send pins in plaintext `attachment_refs`, and a
/// second seat fetches, opens and caches it — the
/// `two_seats_of_a_community_room_exchange_an_attributed_message` shape with a
/// `ResolvedAttachment`, and the end of `send_room_message`'s "not built yet"
/// refusal (`conversation-rooms.md` § The three classes → *Community* →
/// *Attachments — the second content kind*).
#[tokio::test]
async fn two_seats_of_a_community_room_exchange_an_attachment() {
    let png: &[u8] = b"\x89PNG\r\n\x1a\ncommunity-room-bytes";
    // The uniform content handle is the BLAKE3 of the *plaintext*, independent
    // of the seal — the same handle the end-to-end path renders under.
    let blob_hash = hex::encode(blake3::hash(png).as_bytes());

    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2cu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    assert!(
        !alice.engine.has_group(&channel) && !bob.engine.has_group(&channel),
        "a community room has NO MLS group — so no epoch blob key exists to seal under"
    );

    let gen_key = GenerationKey::mint();
    let generation_id = [0x5cu8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    let attachment = ResolvedAttachment {
        blob_hash: blob_hash.clone(),
        filename: "square.png".into(),
        mime_type: "image/png".into(),
        is_image: true,
        bytes: png.to_vec(),
    };
    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "the square, in a picture".into(),
                ..Default::default()
            },
            std::slice::from_ref(&attachment),
        )
        .await
        .expect("a keyed seat can send an attachment into its room");

    // One RoomSealed envelope, one sealed blob, and the plaintext refs name it
    // — the pin is the floor's and works for both classes alike.
    let envelopes = nest.sent_envelopes(&channel.to_string());
    assert_eq!(envelopes.len(), 1, "exactly one channel.send");
    assert!(
        matches!(
            ChannelEnvelope::from_bytes(&envelopes[0]).expect("decode"),
            ChannelEnvelope::RoomSealed { .. }
        ),
        "an attachment send still rides the RoomSealed variant — no new send kind"
    );
    let (sealed_cid_hex, sealed) = {
        let blobs = nest.blobs.lock().unwrap();
        assert_eq!(blobs.len(), 1, "one sealed attachment blob uploaded");
        let (k, v) = blobs.iter().next().unwrap();
        (k.clone(), v.clone())
    };
    assert_ne!(
        sealed.as_slice(),
        png,
        "the blob rests sealed, not plaintext"
    );
    assert_eq!(
        nest.sent_attachment_refs(&channel.to_string()),
        vec![vec![sealed_cid_hex.clone()]],
        "the room send pins its blob in plaintext attachment_refs exactly as the \
         end-to-end path does"
    );

    // THE key assertion: the blob opens under the room's ATTACHMENT kind off
    // the message's generation — not under the message kind, not under any
    // other generation. A second per-kind key, as ruled.
    assert_eq!(
        fauna_mls::room_message::open_room_attachment(&gen_key, &generation_id, &sealed)
            .expect("opens under the attachment kind"),
        png
    );
    assert!(
        fauna_core::group_content::open_group_content(
            &gen_key,
            fauna_core::group_content::ROOM_MESSAGE_CONTENT_KIND,
            &generation_id,
            &sealed,
        )
        .is_err(),
        "the message kind's key does not open an attachment"
    );
    assert!(
        fauna_mls::room_message::open_room_attachment(
            &GenerationKey::mint(),
            &generation_id,
            &sealed
        )
        .is_err(),
        "another generation's key does not either — a removal severs pictures too"
    );

    // Bob polls: the RoomSealed arm fetches + opens + caches the blob, and the
    // bubble names its author exactly as the text path does.
    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let detail = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread");
    let bubble = detail
        .messages
        .iter()
        .find(|m| !attachment_blocks(&m.document).is_empty())
        .expect("an attachment-bearing bubble opened into bob's thread");
    assert_eq!(bubble.body, "the square, in a picture", "caption preserved");
    assert_eq!(
        bubble.sender.person_actor_id(),
        Some(alice.actor),
        "attribution holds on the attachment path too — it is the same signed core"
    );
    let atts = attachment_blocks(&bubble.document);
    assert_eq!(atts.len(), 1);
    assert_eq!(
        atts[0].blob_hash, blob_hash,
        "content handle agrees both ways"
    );
    assert_eq!(atts[0].filename, "square.png");
    assert_eq!(atts[0].mime_type, "image/png");
    assert!(atts[0].is_image);
    assert_eq!(atts[0].size_bytes, png.len() as u64);
    assert_eq!(
        bob.manager.attachment_bytes(blob_hash.clone()).as_deref(),
        Some(png),
        "the bytes round-trip: sealed → uploaded → fetched → opened → cached under the handle"
    );
    assert_eq!(
        bob.poll(&channel, &mut after_seq).await,
        0,
        "cursor prevents re-ingest"
    );
}

/// **A co-member cannot put words in another member's mouth.** Mallory is a
/// real member — she holds the room's generation key like everyone else, so
/// the envelope she seals is well-formed and opens cleanly. Only the author's
/// signature stands between it and a bubble in Alice's name, and this is the
/// reader end of that.
#[tokio::test]
async fn a_co_members_message_in_another_members_name_never_becomes_a_bubble() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2bu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let mallory = room_seat(&nest, channel, "mallory", 3);

    let gen_key = GenerationKey::mint();
    let generation_id = [0x5bu8; 32];
    bob.hold(&gen_key, generation_id);

    // Mallory signs as herself, then relabels the author — the one move a
    // shared-key class cannot refuse with the key alone.
    let mut forged = fauna_mls::room_message::RoomMessageCore {
        room: channel.0.to_vec(),
        generation: generation_id.to_vec(),
        author: mallory.actor,
        sent_at_ms: 1_700_000_000_000,
        body: ChannelMessageBody::Text("I am leaving the room".into()),
    }
    .sign(&ActorKeypair::generate())
    .unwrap();
    forged.core.author = alice.actor;
    let sealed = fauna_core::group_content::seal_group_content(
        &gen_key,
        fauna_core::group_content::ROOM_MESSAGE_CONTENT_KIND,
        &generation_id,
        &fauna_cbor::encode_canonical(&forged).unwrap(),
    )
    .unwrap();
    nest.push_envelope(
        &channel.to_string(),
        ChannelEnvelope::RoomSealed {
            generation: generation_id.to_vec(),
            ciphertext: sealed,
        }
        .to_bytes()
        .unwrap(),
    );

    let mut after_seq = 0i64;
    assert_eq!(
        bob.poll(&channel, &mut after_seq).await,
        0,
        "a message whose signature does not verify under its claimed author \
         must not be rendered as that author's"
    );
    let detail = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread");
    assert!(
        !detail
            .messages
            .iter()
            .any(|m| m.body == "I am leaving the room"),
        "the forged body reached the thread; got {:?}",
        detail.messages.iter().map(|m| &m.body).collect::<Vec<_>>()
    );
}

/// **A seat with no wrap skips the record — honestly, and without stalling the
/// feed.** This is the state the receive walk was already in before the class
/// had a client half (the declared absence), and it stays reachable for good:
/// a member seated under a `history_policy` that retains nothing was never
/// wrapped into the generations it predates. The record stays on the nest log
/// for a session that can open it.
#[tokio::test]
async fn a_seat_holding_no_wrap_skips_the_record_and_keeps_walking() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2cu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let newcomer = room_seat(&nest, channel, "newcomer", 2);

    let gen_key = GenerationKey::mint();
    let generation_id = [0x5cu8; 32];
    alice.hold(&gen_key, generation_id);
    // The newcomer registers no seams at all — the unregistered-backend state.
    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "before your time".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice sends");

    let mut after_seq = 0i64;
    assert_eq!(
        newcomer.poll(&channel, &mut after_seq).await,
        0,
        "no key is a skip, not a failure"
    );
    assert!(
        after_seq >= 1,
        "the walk advances past a record it cannot open — a stalled cursor would \
         wedge the whole feed behind one unopenable message"
    );
}

/// **The wraps are read once per channel, not once per record.** A room's
/// generations are immutable once minted (the id is content-derived from the
/// mint core), so a page of bubbles under one generation must cost one nest
/// round trip and one decapsulation — not one per bubble.
#[tokio::test]
async fn a_page_of_room_bubbles_costs_one_generation_read() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2du8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);

    let gen_key = GenerationKey::mint();
    let generation_id = [0x5du8; 32];
    alice.hold(&gen_key, generation_id);
    let bob_reads = bob.hold(&gen_key, generation_id);

    for n in 0..4 {
        alice
            .backend
            .send(
                &fauna_mls_thread(alice.thread.clone(), vec![]),
                &ComposeState {
                    body_draft: format!("message {n}"),
                    ..Default::default()
                },
                &[],
            )
            .await
            .expect("alice sends");
    }

    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 4);
    assert_eq!(
        bob_reads.calls(),
        1,
        "four bubbles under one generation must cost ONE generation read"
    );
}

// ── The community room's client half, reachable from an app ────────────
//
// The half of the class that needs no new element id: a rotation keeps the
// members' choice about the home nest's read, the read can be granted and
// withdrawn, a
// newcomer's walk waits for its key-in instead of skipping what it is owed, an
// owner's device keys newcomers in off the floor, and the manager founds,
// invites and joins through the doors the apps already call.

/// A generation reader serving, for one roster entry, every generation the
/// scripted ceremony has had published — what `room.generations` answers a
/// member once its own mints have landed. Oldest first, tip last.
struct PublishedGenerations {
    ceremony: Arc<ScriptedCeremony>,
    entry: [u8; 32],
}

#[async_trait]
impl RoomGenerationReader for PublishedGenerations {
    async fn read_generations(
        &self,
        _channel_hex: String,
        _home_nest_url: Option<String>,
    ) -> Option<Vec<RoomGenerationWrap>> {
        let mints = self.ceremony.mints.lock().unwrap();
        let last = mints.len();
        Some(
            mints
                .iter()
                .enumerate()
                .filter_map(|(i, record)| {
                    let GroupGenerationMintRecord::Minted { core, wraps, .. } = record else {
                        return None;
                    };
                    let wrap = wraps.iter().find(|w| w.entry_id == self.entry)?;
                    Some(RoomGenerationWrap {
                        generation_id: group_generation_id(core).ok()?,
                        key_commitment: core.key_commitment,
                        wrap: wrap.wrap.clone(),
                        entry_id: self.entry,
                        is_tip: i + 1 == last,
                    })
                })
                .collect(),
        )
    }
}

/// The roster entries the `n`th published mint wrapped to.
fn mint_entries(ceremony: &ScriptedCeremony, n: usize) -> Vec<[u8; 32]> {
    let mints = ceremony.mints.lock().unwrap();
    let GroupGenerationMintRecord::Minted { wraps, .. } = &mints[n] else {
        panic!("a room generation is published as a Minted record");
    };
    wraps.iter().map(|w| w.entry_id).collect()
}

/// An owner seat over a keyed room whose floor is alice and the home nest,
/// with the nest's row answering `nest_tip_wrapped` — the members' standing
/// choice about its read, as the roster serves it.
fn nest_read_seat(
    nest: &Arc<MockNest>,
    nest_tip_wrapped: Option<bool>,
) -> (RoomSeat, Arc<ScriptedCeremony>, Arc<LiveFloor>) {
    let alice = founder_seat(nest, "alice");
    let ceremony = ScriptedCeremony::new();
    let nest_reception = GroupReceptionKeyRecord::mint(1_700_000_000_002);
    let mut nest_row = room_row(
        ActorId([0x77; 32]),
        RoomPrincipalKind::Nest,
        0xE1,
        &nest_reception,
    );
    nest_row.role = Some(fauna_conversations::room::RoomRole::Member);
    nest_row.tip_wrapped = nest_tip_wrapped;
    let floor = LiveFloor::holding(vec![
        room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception),
        nest_row,
    ]);
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(floor.clone());
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));
    alice.backend.set_room_generation_reader(Arc::new(TipOnly {
        generation_id: [0x6F; 32],
    }));
    (alice, ceremony, floor)
}

/// **An ordinary rotation keeps the members' withdrawal of the nest's read.**
///
/// The nest is deliberately outside the coverage rule so that its grant can be
/// withdrawn — which means nothing at the nest stops a later mint from wrapping
/// to it again. A removal's severance rotation that wrapped to "every target on
/// the floor" would therefore re-grant the withdrawn read on an honest device's
/// ordinary rotation, unasked and unseen by anyone. The roster says whether the
/// tip wraps the nest; a rotation keeps that answer — the nest's own word, which
/// is as far as the revoke reaches (`community-rooms.md` § Implementation status
/// today, the sealing entry → *How far the revoke reaches*: it binds an honest
/// nest, not one that misreports its own row). Unknown (no
/// answer) keeps the status quo — wrap to it — and is neither a revoke nor a re-grant signal.
#[tokio::test]
async fn a_rotation_keeps_the_members_withdrawal_of_the_nests_read() {
    for (answer, nest_wrapped) in [(Some(false), false), (Some(true), true), (None, true)] {
        let nest = Arc::new(MockNest::default());
        let (alice, ceremony, _floor) = nest_read_seat(&nest, answer);
        alice
            .backend
            .rotate_room_key(&ChannelId([0x5A; 32]))
            .await
            .expect("an owner rotates");
        let entries = mint_entries(&ceremony, 0);
        assert!(entries.contains(&[0xA1; 32]), "the owner stays keyed in");
        assert_eq!(
            entries.contains(&[0xE1; 32]),
            nest_wrapped,
            "with the nest's tip wrap answered {answer:?}, the rotation must {} it",
            if nest_wrapped {
                "keep wrapping to"
            } else {
                "keep leaving out"
            }
        );
    }
}

/// **The nest's read is granted and withdrawn by rotation — never by editing
/// the floor.** Withdrawing mints a generation wrapped to every member but the
/// nest (the nest reports its read revoked and deletes its views); granting
/// mints one wrapped to it again. Either way the nest stays on the floor, so
/// the room stays a community room. A room with no nest on its floor has no
/// such read, and the door says so rather than minting anyway.
#[tokio::test]
async fn the_nest_read_is_granted_and_withdrawn_by_rotation() {
    let nest = Arc::new(MockNest::default());
    let (alice, ceremony, floor) = nest_read_seat(&nest, Some(true));
    let channel = ChannelId([0x5A; 32]);

    alice
        .backend
        .rotate_room_nest_read(&channel, false)
        .await
        .expect("an owner withdraws the grant");
    assert!(
        !mint_entries(&ceremony, 0).contains(&[0xE1; 32]),
        "withdrawn: the new generation is not wrapped to the nest"
    );
    alice
        .backend
        .rotate_room_nest_read(&channel, true)
        .await
        .expect("an owner restores the grant");
    assert!(
        mint_entries(&ceremony, 1).contains(&[0xE1; 32]),
        "granted: the new generation is wrapped to the nest again"
    );
    assert_eq!(
        floor.seats().len(),
        2,
        "neither act touched the floor — the nest is still seated"
    );

    floor.unseat(ActorId([0x77; 32]));
    let err = alice
        .backend
        .rotate_room_nest_read(&channel, false)
        .await
        .expect_err("no nest on the floor, no read to withdraw");
    assert!(format!("{err}").contains("seats no home nest"), "{err}");
    assert_eq!(
        ceremony.mints.lock().unwrap().len(),
        2,
        "and the refusal minted nothing"
    );
}

/// **A newcomer's walk waits for its key-in instead of skipping what it is
/// owed.** Acceptance seats a member before any owner or admin has wrapped the
/// room's tip to it, and the tip is the one generation every history policy
/// authorizes — so a message sealed under it in that gap IS the newcomer's to
/// read. Stepping past it, as the walk does for a generation the member was
/// never wrapped into, would lose it for good: the cursor would sit beyond it
/// by the time the key-in lands. So the walk stops before it, says it is
/// waiting (not stalled on a fault), and opens it on the pass after the key-in.
///
/// A generation this member holds no wrap for WHILE holding others is the
/// opposite case — one it predates — and is still stepped past.
#[tokio::test]
async fn a_newcomer_not_yet_keyed_in_waits_before_the_record_instead_of_skipping_it() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2eu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5eu8; 32];
    alice.hold(&gen_key, generation_id);
    // Bob has accepted: his seams are wired, and the room serves him no wrap.
    // One reader for the whole test — the seams are set-once, exactly as a
    // session wires them — so the key-in below is the nest starting to serve
    // his wrap, not a second reader.
    let bob_generations = Arc::new(ScriptedGenerations::new(vec![]));
    bob.backend
        .set_room_generation_reader(bob_generations.clone());
    bob.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![bob.reception.clone()]));

    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "sealed before the key-in".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice sends");

    let mut after_seq = 0i64;
    let outcome = poll_inbound_conv(&bob.backend, &bob.manager, &channel, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(outcome.ingested, 0, "nothing opens before the key-in");
    assert!(
        outcome.stalled && outcome.awaiting_key,
        "the walk stops, and says it is WAITING rather than faulted: {outcome:?}"
    );
    assert_eq!(
        after_seq, 0,
        "the cursor stays before the record it is owed"
    );
    assert!(
        bob.manager
            .thread_detail(bob.thread.clone())
            .expect("thread")
            .room
            .expect("a room")
            .awaiting_key,
        "the room's own row says it is waiting for its key-in, so an app can paint the wait \
         instead of leaving the thread silently empty"
    );

    // The key-in lands: the room now serves bob a wrap for its tip.
    bob_generations
        .wraps
        .lock()
        .unwrap()
        .push(RoomGenerationWrap {
            generation_id,
            key_commitment: group_generation_key_commitment(&gen_key),
            wrap: seal_group_generation_key_to_entry(
                &gen_key,
                &bob.reception.reception_pubkey().unwrap(),
                &generation_id,
                &bob.entry_id,
            )
            .expect("the key-in seals a wrap to bob's reception key"),
            entry_id: bob.entry_id,
            is_tip: true,
        });
    // The first pass after the key-in stops ONCE more — before the record it can
    // now open, ingesting nothing — so the receive loop can reopen the
    // Conversation catch-up window before the room's backlog reaches the index
    // seam (`ConvPollOutcome::keyed_in`).
    let outcome = poll_inbound_conv(&bob.backend, &bob.manager, &channel, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert!(
        outcome.keyed_in && !outcome.stalled && !outcome.awaiting_key,
        "the wait ends as its own answer, not as a strand: {outcome:?}"
    );
    assert_eq!(outcome.ingested, 0, "and it ingests nothing on the way out");
    assert_eq!(
        after_seq, 0,
        "the cursor still sits before the record the next pass will open"
    );
    assert!(
        !bob.manager
            .thread_detail(bob.thread.clone())
            .expect("thread")
            .room
            .expect("a room")
            .awaiting_key,
        "and the room stops saying it is waiting"
    );

    assert_eq!(
        bob.poll(&channel, &mut after_seq).await,
        1,
        "the message sealed before the key-in opens once it has landed"
    );

    // A record under a generation bob was never wrapped into, while he holds
    // one: final, stepped past.
    nest.push_envelope(
        &channel.to_string(),
        ChannelEnvelope::RoomSealed {
            generation: vec![0x99u8; 32],
            ciphertext: vec![0u8; 48],
        }
        .to_bytes()
        .unwrap(),
    );
    let before = after_seq;
    let outcome = poll_inbound_conv(&bob.backend, &bob.manager, &channel, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert!(!outcome.stalled, "not a wait: {outcome:?}");
    assert!(
        after_seq > before,
        "a generation he predates is stepped past"
    );
}

/// The index seam's log, for the room-boundary test below.
#[derive(Debug, PartialEq, Eq, Clone)]
enum IndexEvent {
    /// A message reached the seam, by id.
    Observed(String),
    /// The **Conversation** catch-up boundary closed.
    CatchUpComplete,
    /// The Conversation catch-up window opened again.
    CatchUpReopened,
}

#[derive(Default)]
struct IndexJournal {
    events: Mutex<Vec<IndexEvent>>,
}

impl IndexJournal {
    fn push(&self, e: IndexEvent) {
        self.events.lock().unwrap().push(e);
    }
    fn events(&self) -> Vec<IndexEvent> {
        self.events.lock().unwrap().clone()
    }
}

struct RecordingIndexObserver {
    journal: Arc<IndexJournal>,
}

impl fauna_conversations::index_sink::MessageIndexObserver for RecordingIndexObserver {
    fn observe_indexable_message(
        &self,
        msg: fauna_conversations::index_sink::IndexableMessage<'_>,
    ) {
        self.journal
            .push(IndexEvent::Observed(msg.message_id.0.clone()));
    }
    fn observe_catch_up_complete(&self, kind: fauna_conversations::index_sink::IndexableKind) {
        if kind == fauna_conversations::index_sink::IndexableKind::Conversation {
            self.journal.push(IndexEvent::CatchUpComplete);
        }
    }
    fn observe_catch_up_reopened(&self, kind: fauna_conversations::index_sink::IndexableKind) {
        if kind == fauna_conversations::index_sink::IndexableKind::Conversation {
            self.journal.push(IndexEvent::CatchUpReopened);
        }
    }
}

struct RecordingIndexLauncher {
    journal: Arc<IndexJournal>,
}

#[async_trait]
impl fauna_conversations::backend::IndexBuilderLauncher for RecordingIndexLauncher {
    async fn launch(
        &self,
    ) -> Option<Arc<dyn fauna_conversations::index_sink::MessageIndexObserver>> {
        Some(Arc::new(RecordingIndexObserver {
            journal: Arc::clone(&self.journal),
        })
            as Arc<
                dyn fauna_conversations::index_sink::MessageIndexObserver,
            >)
    }

    /// No third-ingest-class arm on this fake launcher. Written out because the
    /// trait requires it, so a real launcher cannot silently skip its walk.
    async fn corpus_changed(&self, _corpus: fauna_conversations::backend::NestCorpus) {}
}

/// Deadline-poll a journalled predicate: a generous ceiling and a causal fact,
/// never a settle-sleep (`e2e-conventions.md` convention 14).
async fn await_index(journal: &IndexJournal, done: impl Fn(&[IndexEvent]) -> bool, what: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let events = journal.events();
        if done(&events) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; journal so far: {events:?}"
        );
        // sleep-ok: the poll interval of a deadline wait, not the assertion.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// **One unkeyed room does not hold the ACCOUNT's Conversation catch-up open —
/// and its key-in reopens that window before the room's backlog is indexed.**
///
/// A newcomer's walk stops before the first record it holds no key for, and
/// that stop is reported `stalled`. Counted as an unfinished fold it took the
/// whole account with it: `poll_bound` answered "incomplete" every sweep, so
/// `observe_catch_up_complete(Conversation)` never fired for as long as ONE room
/// went unkeyed — and the wait has no bound, no timeout and nothing that ends it
/// but an owner's or admin's device happening to poll. A seat that had stood
/// down from the index lease then withheld every *other* channel's backlog for
/// the rest of the session.
///
/// So an awaiting room folds for now, and the boundary closes on its siblings.
/// The room's own backlog is not thereby reclassified: the walk stops once more
/// when the key finally lands, the loop reopens the window, and only then walks
/// the room — so what the wait held back arrives as backlog, in the order a
/// launch-time arm sees (`content-index-ingest.md` § Ingest triggers, v1 → *A
/// community room waiting for its key-in*).
///
/// Mutations this reddens under: counting `awaiting_key` as an unfinished fold
/// (no boundary at all); dropping the `keyed_in` stop or the reopen (the room's
/// backlog arrives after the boundary, as trickle); reopening after the re-walk
/// instead of before it (same order break).
#[tokio::test(flavor = "multi_thread")]
async fn an_unkeyed_room_folds_for_now_and_its_key_in_reopens_the_catch_up_window() {
    let nest = Arc::new(MockNest::default());
    let room = ChannelId([0x2du8; 32]);
    let alice = room_seat(&nest, room, "alice", 0xA1);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5du8; 32];
    alice.hold(&gen_key, generation_id);

    // Bob's own session over the same nest: the room he has accepted and nobody
    // has keyed him into, plus an ordinary channel whose fold is clean — the
    // sibling whose backlog the boundary is about.
    let bob_engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_actor = bob_engine.identity_actor_id();
    let push = Arc::new(MockPush {
        events: Mutex::new(VecDeque::new()),
    });
    let session = ConversationsSession::from_parts(
        bob_engine,
        nest.clone(),
        "bob".into(),
        bob_actor,
        Some(push.clone()),
    );
    let manager = session.manager();
    let backend = session.backend();
    let reception = GroupReceptionKeyRecord::mint(1_700_000_000_007);
    let bob_entry = [0xB7u8; 32];
    // Bob has accepted: his seams are wired, and the room serves him no wrap.
    let bob_generations = Arc::new(ScriptedGenerations::new(vec![]));
    backend.set_room_generation_reader(bob_generations.clone());
    backend.set_group_reception_keys(HeldReceptionKeys::holding(vec![reception.clone()]));

    let setup = |label: &str, actor: ActorId, id: &str| {
        manager
            .ingest_inbound(RailInboundMessage {
                rail: Rail::FaunaMls,
                sender: fauna_addr(label, actor),
                recipients: vec![],
                subject: None,
                body: "<<setup>>".into(),
                body_format: BodyFormat::PlainText,
                timestamp_ms: 1,
                message_id: MessageId(id.into()),
                in_reply_to: None,
                attachments: vec![],
                badges: MessageBadges::default(),
                legal_takedown_ref: None,
                plane_ref: None,
            })
            .expect("setup ingest");
    };
    setup("alice", alice.actor, "setup-room");
    let room_thread = manager.snapshot().threads[0].thread_id.clone();
    setup("carol", ActorId([0xC7; 32]), "setup-plain");
    let plain_thread = manager
        .snapshot()
        .threads
        .iter()
        .map(|t| t.thread_id.clone())
        .find(|t| *t != room_thread)
        .expect("a second thread");
    backend.bind_channel(room_thread.clone(), room);
    backend.bind_channel(plain_thread, ChannelId([0x4du8; 32]));

    let journal = Arc::new(IndexJournal::default());
    session.set_index_builder_launcher(Arc::new(RecordingIndexLauncher {
        journal: Arc::clone(&journal),
    }));

    // Sealed under the tip before bob's loop ever runs: his first sweep meets a
    // record he holds no key for.
    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "sealed before the key-in".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice sends");

    session.start_receive_loop().await;

    await_index(
        &journal,
        |events| events.contains(&IndexEvent::CatchUpComplete),
        "the conversation boundary to close while one room still waits for its key-in",
    )
    .await;
    assert!(
        manager
            .thread_detail(room_thread.clone())
            .expect("thread")
            .room
            .expect("a room")
            .awaiting_key,
        "the boundary closed because the room folded FOR NOW — it is still waiting, and says so"
    );

    // The key-in lands: the room now serves bob a wrap for its tip.
    bob_generations
        .wraps
        .lock()
        .unwrap()
        .push(RoomGenerationWrap {
            generation_id,
            key_commitment: group_generation_key_commitment(&gen_key),
            wrap: seal_group_generation_key_to_entry(
                &gen_key,
                &reception.reception_pubkey().unwrap(),
                &generation_id,
                &bob_entry,
            )
            .expect("the key-in seals a wrap to bob's reception key"),
            entry_id: bob_entry,
            is_tip: true,
        });
    push.events
        .lock()
        .unwrap()
        .push_back(ConvPushEvent::ChannelMessage);

    let prefix = format!("conv:{room}:");
    let opened = |events: &[IndexEvent]| {
        events
            .iter()
            .any(|e| matches!(e, IndexEvent::Observed(id) if id.starts_with(&prefix)))
    };
    await_index(
        &journal,
        opened,
        "the room's backlog to reach the index seam once the key-in landed",
    )
    .await;
    await_index(
        &journal,
        |events| {
            events
                .iter()
                .filter(|e| **e == IndexEvent::CatchUpComplete)
                .count()
                >= 2
        },
        "the reopened window to close again",
    )
    .await;

    let events = journal.events();
    let reopened = events
        .iter()
        .position(|e| *e == IndexEvent::CatchUpReopened)
        .expect("the key-in must reopen the Conversation window");
    let observed = events
        .iter()
        .position(|e| matches!(e, IndexEvent::Observed(id) if id.starts_with(&prefix)))
        .expect("the room's message must reach the seam");
    let closed_again = events
        .iter()
        .enumerate()
        .filter(|(_, e)| **e == IndexEvent::CatchUpComplete)
        .map(|(i, _)| i)
        .nth(1)
        .expect("the reopened window must close again");
    assert!(
        reopened < observed && observed < closed_again,
        "the room's held-back backlog must arrive INSIDE a reopened window — reopen, then the \
         backlog, then the boundary, the order a launch-time arm sees. Journal: {events:?}"
    );
    assert!(
        !manager
            .thread_detail(room_thread)
            .expect("thread")
            .room
            .expect("a room")
            .awaiting_key,
        "and the room stops saying it waits"
    );
}

/// **An owner's device keys in a member the floor seated with no wrap** — the
/// inviter's half of the join, which until now nothing ran: the room plane had
/// the door (`key_in_room_member`) and no caller, so an accepted invitee read
/// nothing, ever. The same pass seats the newcomer as a participant, since no
/// walk this device folds carries an acceptance.
#[tokio::test]
async fn an_owners_device_keys_in_a_member_the_floor_seated_without_a_wrap() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2fu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.hold(&GenerationKey::mint(), [0x5fu8; 32]);

    let bob = ActorId([0xB0; 32]);
    let mut alice_row = room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception);
    alice_row.tip_wrapped = Some(true);
    let mut bob_row = room_row(
        bob,
        RoomPrincipalKind::User,
        0xB1,
        &GroupReceptionKeyRecord::mint(1_700_000_000_003),
    );
    bob_row.role = Some(fauna_conversations::room::RoomRole::Member);
    bob_row.handle = Some("bob".into());
    bob_row.domain = Some("nest.test".into());
    bob_row.tip_wrapped = Some(false);
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![alice_row, bob_row]));

    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    {
        let backfills = ceremony.backfills.lock().unwrap();
        assert_eq!(backfills.len(), 1, "one key-in, for the one member owed it");
        assert_eq!(backfills[0].1, hex::encode(bob.0), "and it is bob's");
    }
    let detail = alice
        .manager
        .thread_detail(alice.thread.clone())
        .expect("thread");
    let seated = detail
        .participants
        .iter()
        .find(|p| p.person_actor_id() == Some(bob))
        .expect("the floor's newcomer is seated as a participant");
    assert_eq!(
        seated.person_handle(),
        Some("bob@nest.test"),
        "and named from the same read"
    );
}

/// **A plain member's device never keys anyone in** — key authority is the
/// owner's and the admins' (`conversation-rooms.md` § Don't do these), and the
/// nest would refuse the top-up anyway. Nor does an owner re-key a member the
/// tip already covers.
#[tokio::test]
async fn a_plain_members_device_keys_nobody_in() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x30u8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.hold(&GenerationKey::mint(), [0x60u8; 32]);

    let owed = |actor: ActorId, seed: u8| {
        let mut row = room_row(
            actor,
            RoomPrincipalKind::User,
            seed,
            &GroupReceptionKeyRecord::mint(1_700_000_000_004),
        );
        row.role = Some(fauna_conversations::room::RoomRole::Member);
        row.tip_wrapped = Some(false);
        row
    };
    let mut alice_row = room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception);
    alice_row.role = Some(fauna_conversations::room::RoomRole::Member);
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![
            alice_row,
            owed(ActorId([0xC0; 32]), 0xC1),
        ]));
    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;
    assert!(
        ceremony.backfills.lock().unwrap().is_empty(),
        "a plain member holds no key authority"
    );
}

/// **An owner keys in only a member the tip is KNOWN not to cover** — a
/// `tip_wrapped` answer of `Some(false)`. A member the tip covers is never
/// re-keyed, and a `user` row answering `None` is not keyed in either: `None`
/// means the nest did not say, and wrapping to every unknown row on every poll
/// is the re-wrap the field exists to prevent. Nor is such a room refused — it
/// waits, alone and visibly (`community-rooms.md` § Implementation status today
/// → *The wait is scoped to the room, and the room says it is waiting*).
///
/// Mutations this reddens under: keying in on anything but `Some(true)` (the
/// unknown row is keyed in); dropping the `tip_wrapped` test altogether (the
/// covered row is re-keyed as well). Neither reddened any test before this one:
/// the owner's test seats one `Some(false)` newcomer, and the plain member's
/// holds no key authority whatever the rows answer.
#[tokio::test]
async fn an_owners_device_keys_in_only_a_member_the_tip_is_known_not_to_cover() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x31u8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.hold(&GenerationKey::mint(), [0x61u8; 32]);

    let member = |actor: ActorId, seed: u8, tip_wrapped: Option<bool>| {
        let mut row = room_row(
            actor,
            RoomPrincipalKind::User,
            seed,
            &GroupReceptionKeyRecord::mint(1_700_000_000_005),
        );
        row.role = Some(fauna_conversations::room::RoomRole::Member);
        row.tip_wrapped = tip_wrapped;
        row
    };
    let owed = ActorId([0xB2; 32]);
    let mut alice_row = room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception);
    alice_row.tip_wrapped = Some(true);
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![
            alice_row,
            member(owed, 0xB3, Some(false)),
            member(ActorId([0xC2; 32]), 0xC3, Some(true)),
            member(ActorId([0xD2; 32]), 0xD3, None),
        ]));
    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    let keyed: Vec<String> = ceremony
        .backfills
        .lock()
        .unwrap()
        .iter()
        .map(|(_, target, _)| target.clone())
        .collect();
    assert_eq!(
        keyed,
        vec![hex::encode(owed.0)],
        "exactly one key-in, for the member the tip is known not to cover — none for a covered \
         member, and none for one the nest gave no answer about"
    );
}

// ── The tend pass heals this account's OWN seat ─────────────────────────────
//
// The client half of `fauna.conversations.room.set_reception_key`
// (`community-rooms.md` § Implementation status today, *A seat gains or
// rotates its wrap target*): before an owner's device keys anybody else in, it
// makes its own seat keyable and keyed to the account's current wrap target.

/// A floor row for `seat` as the home nest serves it: keyed to `reception`
/// (`None` = the keyless seat a succession ceremony writes), at `seat`'s own
/// entry, holding `role`, with the tip's coverage as the nest reports it.
fn own_row(
    seat: &RoomSeat,
    reception: Option<&GroupReceptionKeyRecord>,
    role: fauna_conversations::room::RoomRole,
    tip_wrapped: Option<bool>,
) -> RoomRosterKnownMember {
    let mut row = room_row(
        seat.actor,
        RoomPrincipalKind::User,
        seat.entry_id[0],
        &seat.reception,
    );
    row.reception_pubkey = reception.map(|r| r.reception_pubkey().unwrap());
    row.role = Some(role);
    row.tip_wrapped = tip_wrapped;
    row
}

/// The home nest's row, keyed and reading the room.
fn nest_row() -> RoomRosterKnownMember {
    let mut row = room_row(
        ActorId([0x77; 32]),
        RoomPrincipalKind::Nest,
        0xB2,
        &GroupReceptionKeyRecord::mint(1_700_000_000_006),
    );
    row.role = None;
    row.tip_wrapped = Some(true);
    row
}

/// Have the scripted nest bind the key on `floor` when the door is called —
/// what a real floor does, and what the mint that follows reads.
fn bind_on_floor(ceremony: &ScriptedCeremony, floor: &Arc<LiveFloor>, who: ActorId) {
    let floor = floor.clone();
    *ceremony.on_set_key.lock().unwrap() = Some(Box::new(move |_, key| floor.rekey(who, key)));
}

/// **A keyless governing seat supplies this account's key and mints itself
/// back in.** The successor's shape: seated as owner with no wrap target, and
/// the tip does not cover it. The pass binds the account's current key through
/// the door, then — because the seat governs and the door named a tip it holds
/// no wrap for — mints a fresh generation parented on that tip, over the floor
/// as the nest now serves it: wrapped to the seat it just keyed, the nest's
/// read kept. The generations read shows an uncovered seat nothing, which is
/// why the parent comes from the door's answer.
#[tokio::test]
async fn a_keyless_governing_seat_supplies_its_key_and_mints_itself_back_in() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x32u8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    let tip = [0x62u8; 32];
    alice.hold(&GenerationKey::mint(), tip);
    *ceremony.answer_uncovered_tip.lock().unwrap() = Some(tip);
    let floor = LiveFloor::holding(vec![
        own_row(
            &alice,
            None,
            fauna_conversations::room::RoomRole::Owner,
            Some(false),
        ),
        nest_row(),
    ]);
    alice.backend.set_room_roster_reader(floor.clone());
    bind_on_floor(&ceremony, &floor, alice.actor);

    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    let set = ceremony.set_keys.lock().unwrap().clone();
    assert_eq!(set.len(), 1, "one call, for this account's own seat");
    assert_eq!(set[0].0, channel.to_string());
    assert_eq!(
        set[0].1,
        alice.reception.reception_pubkey().unwrap(),
        "the account's current wrap target"
    );
    let (_, wraps, _, parents) = ceremony.published();
    assert_eq!(
        parents,
        vec![tip],
        "parented on the tip the door named — the only way an uncovered seat learns it"
    );
    assert!(
        wraps.iter().any(|w| w.entry_id == alice.entry_id),
        "wrapped to the seat it just keyed"
    );
    assert!(
        wraps.iter().any(|w| w.entry_id == [0xB2; 32]),
        "and the home nest's read is kept, as every ordinary rotation keeps it"
    );
}

/// **A keyless plain member supplies its key and mints nothing** — key
/// authority is the owner's and the admins', and an owner's or admin's own
/// tend pass keys the now-keyable seat in, the way it covers any newcomer.
#[tokio::test]
async fn a_keyless_plain_member_supplies_its_key_and_mints_nothing() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x33u8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    let tip = [0x63u8; 32];
    alice.hold(&GenerationKey::mint(), tip);
    *ceremony.answer_uncovered_tip.lock().unwrap() = Some(tip);
    let floor = LiveFloor::holding(vec![
        own_row(
            &alice,
            None,
            fauna_conversations::room::RoomRole::Member,
            Some(false),
        ),
        nest_row(),
    ]);
    alice.backend.set_room_roster_reader(floor.clone());
    bind_on_floor(&ceremony, &floor, alice.actor);

    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    assert_eq!(
        ceremony.set_keys.lock().unwrap().len(),
        1,
        "the seat is made keyable"
    );
    assert!(
        ceremony.mints.lock().unwrap().is_empty(),
        "a plain member holds no key authority — an admin's pass covers it"
    );
}

/// **A seat keyed to the account's current key costs the pass nothing** —
/// covered, or of unknown coverage (no answer is never "no").
/// Only a governing seat the tip is KNOWN not to cover re-asks the door, and
/// mints on its answer: a mint that failed on an earlier pass has no other way
/// to learn the parent it must name now.
#[tokio::test]
async fn a_current_seat_costs_nothing_unless_it_governs_and_is_known_uncovered() {
    let nest = Arc::new(MockNest::default());
    let tip = [0x64u8; 32];
    // One fresh seat per case: the floor cadence admits one pass per channel
    // per session, which is the production shape and not worth faking.
    let mut seat_no = 0u8;
    let mut pass = |role: fauna_conversations::room::RoomRole, tip_wrapped: Option<bool>| {
        seat_no += 1;
        let channel = ChannelId([0x34u8 + seat_no; 32]);
        let alice = room_seat(&nest, channel, "alice", 0xA1);
        let ceremony = ScriptedCeremony::new();
        alice.backend.set_room_ceremony(ceremony.clone());
        alice.hold(&GenerationKey::mint(), tip);
        *ceremony.answer_uncovered_tip.lock().unwrap() = Some(tip);
        alice
            .backend
            .set_room_roster_reader(LiveFloor::holding(vec![
                own_row(&alice, Some(&alice.reception), role, tip_wrapped),
                nest_row(),
            ]));
        async move {
            alice
                .backend
                .tend_community_room(&alice.manager, &channel)
                .await;
            ceremony
        }
    };

    for (role, tip_wrapped) in [
        (fauna_conversations::room::RoomRole::Owner, Some(true)),
        (fauna_conversations::room::RoomRole::Owner, None),
        (fauna_conversations::room::RoomRole::Member, Some(false)),
    ] {
        let ceremony = pass(role, tip_wrapped).await;
        assert!(
            ceremony.set_keys.lock().unwrap().is_empty(),
            "nothing to do for {role:?} with tip_wrapped={tip_wrapped:?}"
        );
        assert!(ceremony.mints.lock().unwrap().is_empty());
    }

    let ceremony = pass(fauna_conversations::room::RoomRole::Owner, Some(false)).await;
    assert_eq!(
        ceremony.set_keys.lock().unwrap().len(),
        1,
        "a governing seat the tip is known not to cover re-asks the door"
    );
    let (_, _, _, parents) = ceremony.published();
    assert_eq!(parents, vec![tip], "and mints on its answer");
}

/// **A rotated governing seat re-binds its current key and rotates the room.**
/// The floor holds the key of an earlier moment; the account's current key is
/// another. The pass binds the current one (the door reports a rotation of a
/// still-covered seat) and — trigger (2) of the scheme, the member's
/// fleet-severance signal — mints the room's next generation, naming the tip
/// it can still see because the old wrap is still served at its entry.
#[tokio::test]
async fn a_rotated_governing_seat_rebinds_its_key_and_rotates_the_room() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x35u8; 32]);
    let alice = room_seat(&nest, channel, "alice", 0xA1);
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    let tip = [0x65u8; 32];
    alice.hold(&GenerationKey::mint(), tip);
    *ceremony.answer_rotated.lock().unwrap() = true;
    let stale = GroupReceptionKeyRecord::mint(1_600_000_000_000);
    let floor = LiveFloor::holding(vec![
        own_row(
            &alice,
            Some(&stale),
            fauna_conversations::room::RoomRole::Owner,
            Some(true),
        ),
        nest_row(),
    ]);
    alice.backend.set_room_roster_reader(floor.clone());
    bind_on_floor(&ceremony, &floor, alice.actor);

    alice
        .backend
        .tend_community_room(&alice.manager, &channel)
        .await;

    let set = ceremony.set_keys.lock().unwrap().clone();
    assert_eq!(set.len(), 1);
    assert_eq!(
        set[0].1,
        alice.reception.reception_pubkey().unwrap(),
        "the current key replaces the stale one"
    );
    let (_, wraps, _, parents) = ceremony.published();
    assert_eq!(
        parents,
        vec![tip],
        "the rotation names the tip it can still see"
    );
    assert!(
        wraps.iter().any(|w| w.entry_id == alice.entry_id),
        "and seals the next generation to the re-bound seat"
    );
}

/// **The composer founds a community room when the home nest is chosen** — and
/// through the doors every app already calls: the picker's toggle, then Send.
///
/// Everything a founding owes happens in that one gesture: the ceremony with
/// the typed topic as the room's name, a first mint that wraps to the home
/// nest (the grant the class is), one invitation per recipient, a thread keyed
/// by the room's channel (so a plain conversation with the same people stays a
/// different thread), and the first message sealed under the room's key. No
/// MLS group is created anywhere — that is what makes it this class.
#[tokio::test]
async fn the_composer_founds_a_community_room_when_the_home_nest_is_chosen() {
    use fauna_conversations::room::{RoomClass, prospective_room_class};
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let ceremony = ScriptedCeremony::new();
    let nest_reception = GroupReceptionKeyRecord::mint(1_700_000_000_005);
    let floor = LiveFloor::holding(vec![
        room_row(alice.actor, RoomPrincipalKind::User, 0xA1, &alice.reception),
        room_row(
            ActorId([0x77; 32]),
            RoomPrincipalKind::Nest,
            0xB2,
            &nest_reception,
        ),
    ]);
    alice.backend.set_room_ceremony(ceremony.clone());
    alice.backend.set_room_roster_reader(floor.clone());
    alice
        .backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![alice.reception.clone()]));
    alice
        .backend
        .set_room_generation_reader(Arc::new(PublishedGenerations {
            ceremony: ceremony.clone(),
            entry: [0xA1; 32],
        }));

    let carol = ActorId([0xC0; 32]);
    let m = &alice.manager;
    m.start_new_conversation();
    m.accept_new_thread_chip(fauna_addr("carol", carol));
    let picker = || {
        m.snapshot()
            .new_thread_compose
            .and_then(|c| c.recipient_picker)
            .expect("the picker is open")
    };
    assert_eq!(
        prospective_room_class(&picker().chips, picker().include_home_nest),
        Some(RoomClass::EndToEnd),
        "a Fauna chip alone is an end-to-end room"
    );
    m.set_new_thread_home_nest(true);
    assert!(
        picker().include_home_nest,
        "the toggle is the composer's state"
    );
    assert_eq!(
        prospective_room_class(&picker().chips, picker().include_home_nest),
        Some(RoomClass::Community),
        "and the picker says so before the first message"
    );
    m.set_new_thread_subject(Some("The Commons".into()));
    m.set_new_thread_body("the square is open".into());

    let id = m
        .send_new_thread()
        .await
        .expect("the room is founded and the message sent")
        .expect("a thread");

    assert_eq!(ceremony.creates(), 1, "one birth ceremony");
    assert_eq!(
        ceremony.creates.lock().unwrap()[0]
            .policy
            .policy
            .name
            .as_deref(),
        Some("The Commons"),
        "the typed topic names the room — everyone who joins sees it"
    );
    assert!(
        mint_entries(&ceremony, 0).contains(&[0xB2; 32]),
        "the founding mint wraps to the home nest — the grant the class is"
    );
    {
        let invites = ceremony.invites.lock().unwrap();
        assert_eq!(invites.len(), 1, "one invitation per recipient");
        assert_eq!(invites[0].0.invite.invitee, carol);
    }
    let channel_hex = RailBackend::channel_binding_hex(alice.backend.as_ref(), &id)
        .expect("the thread is bound to the room");
    assert_eq!(
        m.channel_hex(&id).as_deref(),
        Some(channel_hex.as_str()),
        "keyed by the room's channel, not by its recipients"
    );
    let channel =
        ChannelId(<[u8; 32]>::try_from(hex::decode(&channel_hex).unwrap().as_slice()).unwrap());
    assert!(
        !alice.engine.has_group(&channel),
        "no MLS group — the room is born by the ceremony"
    );
    let envelopes = nest.sent_envelopes(&channel_hex);
    assert_eq!(envelopes.len(), 1, "the first message went out");
    assert!(
        matches!(
            ChannelEnvelope::from_bytes(&envelopes[0]).expect("decode"),
            ChannelEnvelope::RoomSealed { .. }
        ),
        "sealed under the room's generation key"
    );
    let snapshot = m.snapshot();
    assert_eq!(snapshot.selected_thread_id.as_ref(), Some(&id));
    assert!(snapshot.new_thread_compose.is_none(), "the composer closed");

    // Inviting from the thread header is the same invitation — and it seats
    // nobody, so no chip appears for the invitee before they accept.
    alice.backend.tend_community_room(m, &channel).await;
    let dave = ActorId([0xD0; 32]);
    m.open_add_participant(id.clone());
    m.accept_add_participant_chip(fauna_addr("dave", dave));
    m.confirm_add_participant().await;
    assert_eq!(
        m.page_error_diagnostic(),
        None,
        "the invitation went out cleanly"
    );
    assert_eq!(
        ceremony.invites.lock().unwrap().len(),
        2,
        "dave was invited"
    );
    assert!(
        !m.thread_detail(id)
            .expect("thread")
            .participants
            .iter()
            .any(|p| p.person_actor_id() == Some(dave)),
        "an invitation is not a seating: no optimistic chip for dave"
    );
}

/// **A standing invitation is listed, accepted, and opens its room** — the
/// invitee's half, through the manager doors the list's accept button calls.
/// Declining settles it and tells the room nothing.
#[tokio::test]
async fn an_invitation_is_listed_accepted_and_opens_its_room() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let bob = founder_seat(&nest, "bob");
    // One nest: the ceremony both seats talk to.
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![room_row(
            alice.actor,
            RoomPrincipalKind::User,
            0xA1,
            &alice.reception,
        )]));
    bob.backend.set_room_ceremony(ceremony.clone());
    bob.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![bob.reception.clone()]));

    let room = ChannelId([0x3au8; 32]);
    let other_room = ChannelId([0x3bu8; 32]);
    for channel in [room, other_room] {
        alice
            .backend
            .invite_to_room(
                &channel,
                bob.actor,
                fauna_mls::room_policy::RoomRole::Member,
                None,
            )
            .await
            .expect("alice invites bob");
    }

    bob.manager.refresh_room_invitations().await;
    let listed = bob.manager.snapshot().room_invitations;
    assert_eq!(listed.len(), 2, "both invitations stand");
    assert!(
        listed
            .iter()
            .all(|i| i.role == fauna_conversations::room::RoomRole::Member
                && !i.inviter_display.is_empty()),
        "each names the rank on offer and who is asking: {listed:?}"
    );

    let thread = bob
        .manager
        .accept_room_invitation(listed[0].id)
        .await
        .expect("bob accepts");
    {
        let accepts = ceremony.accepts.lock().unwrap();
        assert_eq!(accepts.len(), 1, "one seating");
        assert_eq!(
            accepts[0].0,
            room.to_string(),
            "into the room he was invited to"
        );
    }
    assert_eq!(
        RailBackend::channel_binding_hex(bob.backend.as_ref(), &thread),
        Some(room.to_string()),
        "the room opens as a thread bound to its channel"
    );
    let snapshot = bob.manager.snapshot();
    assert_eq!(snapshot.selected_thread_id.as_ref(), Some(&thread));
    assert_eq!(
        snapshot.room_invitations.len(),
        1,
        "the accepted invitation left the list in the same act"
    );

    bob.manager
        .decline_room_invitation(snapshot.room_invitations[0].id)
        .await;
    assert!(bob.manager.snapshot().room_invitations.is_empty());
    assert!(
        ceremony.deliveries.lock().unwrap().is_empty(),
        "both invitations are settled"
    );
    assert_eq!(
        ceremony.accepts.lock().unwrap().len(),
        1,
        "and the decline seated nobody"
    );
}

/// **A lapsed invitation leaves the list in the act that refused it.** The
/// accept door judges an invitation again and consumes one its inviter could
/// no longer issue (`conversation-rooms.md` § Join rules and invites), so
/// after such a refusal nothing stands behind the row on screen — left there,
/// its accept button could only fail until the next background sweep.
#[tokio::test]
async fn a_lapsed_invitation_leaves_the_list_when_its_accept_is_refused() {
    let nest = Arc::new(MockNest::default());
    let alice = founder_seat(&nest, "alice");
    let bob = founder_seat(&nest, "bob");
    let ceremony = ScriptedCeremony::new();
    alice.backend.set_room_ceremony(ceremony.clone());
    alice
        .backend
        .set_room_roster_reader(LiveFloor::holding(vec![room_row(
            alice.actor,
            RoomPrincipalKind::User,
            0xA1,
            &alice.reception,
        )]));
    bob.backend.set_room_ceremony(ceremony.clone());
    bob.backend
        .set_group_reception_keys(HeldReceptionKeys::holding(vec![bob.reception.clone()]));

    let room = ChannelId([0x3cu8; 32]);
    let other_room = ChannelId([0x3du8; 32]);
    for channel in [room, other_room] {
        alice
            .backend
            .invite_to_room(
                &channel,
                bob.actor,
                fauna_mls::room_policy::RoomRole::Member,
                None,
            )
            .await
            .expect("alice invites bob");
    }
    bob.manager.refresh_room_invitations().await;
    let listed = bob.manager.snapshot().room_invitations;
    assert_eq!(listed.len(), 2);

    *ceremony.lapse_on_accept.lock().unwrap() = Some("this invitation has lapsed".to_string());
    // Delivery handles are monotonic, so the first invitation — `room`'s — is
    // the lower one; the snapshot itself names no room.
    let lapsed = listed.iter().map(|i| i.id).min().unwrap();
    let standing = listed.iter().map(|i| i.id).max().unwrap();
    assert!(
        bob.manager.accept_room_invitation(lapsed).await.is_none(),
        "the nest refused it"
    );

    let snapshot = bob.manager.snapshot();
    assert!(
        bob.manager.page_error_diagnostic().is_some(),
        "the refusal is on the page"
    );
    assert_eq!(
        snapshot
            .room_invitations
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![standing],
        "the lapsed invitation is gone and the other still stands"
    );
    assert!(
        ceremony.accepts.lock().unwrap().is_empty(),
        "nobody was seated"
    );
}

// ── Room-restricted posts: the room's keys, answered for the feed ──────
//
// A room-restricted post is its author's ordinary post, sealed under a base the
// room's class decides (`ui/feed.md` § Encryption at rest → *Room-restricted —
// the ruling*). The feed holds no room keys, so it asks the conversations
// plane through `fauna_core::room_post::RoomPostKeys` — these pin that answer.

use fauna_core::room_post::{RoomPostKeys, RoomPostSeal};

/// An **end-to-end** room: the author seals at its group's current epoch, and a
/// member at that epoch derives the same base. A member asked about an epoch it
/// never held is refused rather than handed some other epoch's key.
#[tokio::test]
async fn an_end_to_end_room_post_seals_and_opens_at_the_groups_epoch() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
    let _ = bob.join_from_welcome(welcome).unwrap();
    let nest = Arc::new(MockNest::default());
    let alice_backend = FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice.identity_actor_id(),
    );
    let bob_backend = FaunaMlsBackend::new(bob.clone(), nest, "bob", bob.identity_actor_id());

    let (seal, base) = alice_backend
        .room_post_seal_key(channel.0)
        .await
        .expect("a member of an end-to-end room seals");
    let RoomPostSeal::EndToEnd { epoch } = seal else {
        panic!("an MLS group is an end-to-end room, got {seal:?}");
    };
    assert_eq!(epoch, alice.current_epoch(&channel).unwrap());
    assert_eq!(
        *bob_backend
            .room_post_base_key(channel.0, seal)
            .await
            .expect("a member at that epoch opens"),
        *base
    );
    assert!(
        bob_backend
            .room_post_base_key(channel.0, RoomPostSeal::EndToEnd { epoch: epoch + 7 })
            .await
            .is_err(),
        "an epoch this member never held is not answered with another one"
    );
}

/// An end-to-end room's **removed** member, once its device has processed the
/// removal, is no longer seated on it (`RailBackend::seated_on_room`, what the
/// composer's room offer filters on) and can seal no new post for it — the
/// exporter refuses an evicted group — while the base of a post sealed at the
/// epoch it was removed at still answers on both sides, with no expiry
/// (`ui/feed.md` ruling 6, and the ruling-5 residue its owner states).
#[tokio::test]
async fn a_removed_member_is_unseated_and_seals_nothing_new_but_keeps_its_last_epoch() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob_kps = bob.generate_key_packages(1).unwrap();
    let (channel, welcome) = alice.create_group(&bob_kps).unwrap();
    let _ = bob.join_from_welcome(welcome).unwrap();
    let nest = Arc::new(MockNest::default());
    let alice_backend = FaunaMlsBackend::new(
        alice.clone(),
        nest.clone(),
        "alice",
        alice.identity_actor_id(),
    );
    let bob_backend = FaunaMlsBackend::new(bob.clone(), nest, "bob", bob.identity_actor_id());
    let thread = fauna_mls_thread(
        ThreadId("room".into()),
        vec![
            TypedAddress::Fauna {
                handle: "alice".into(),
                actor_id: alice.identity_actor_id(),
            },
            TypedAddress::Fauna {
                handle: "bob".into(),
                actor_id: bob.identity_actor_id(),
            },
        ],
    );
    alice_backend.bind_channel(thread.thread_id.clone(), channel);
    bob_backend.bind_channel(thread.thread_id.clone(), channel);

    // Sealed while bob is seated: the base both hold.
    let (seal, base) = bob_backend
        .room_post_seal_key(channel.0)
        .await
        .expect("a seated member seals");
    assert!(bob_backend.seated_on_room(&thread));

    let bob_leaf = alice
        .find_leaf_by_identity(&channel, &bob.identity_actor_id())
        .expect("bob's leaf");
    let commit = alice
        .remove_member(&channel, bob_leaf)
        .expect("remove commit");
    bob.process_commit(&channel, &commit)
        .expect("bob processes his own removal");

    assert!(
        !bob_backend.seated_on_room(&thread),
        "a device that processed its removal is no longer seated"
    );
    assert!(
        alice_backend.seated_on_room(&thread),
        "the remaining member is"
    );
    assert!(
        bob_backend.room_post_seal_key(channel.0).await.is_err(),
        "an evicted group seals no new post"
    );
    assert_eq!(
        *bob_backend
            .room_post_base_key(channel.0, seal)
            .await
            .expect("the removed member keeps the epoch it was removed at"),
        *base
    );
    assert_eq!(
        *alice_backend
            .room_post_base_key(channel.0, seal)
            .await
            .expect("the remaining member keeps it too — no freshness check"),
        *base
    );
}

/// A **community** room: the author seals under the tip generation, the base
/// is the post kind's content key over it (never the message kind's), and
/// another member holding that generation's wrap derives the same base.
#[tokio::test]
async fn a_community_room_post_seals_under_the_tip_and_opens_by_its_generation() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0xC7; 32]);
    let gen_key = GenerationKey::from_bytes([0x3C; 32]);
    let generation = [0x9A; 32];
    let author = room_seat(&nest, channel, "author", 1);
    let reader = room_seat(&nest, channel, "reader", 2);
    author.hold(&gen_key, generation);
    reader.hold(&gen_key, generation);

    let (seal, base) = author
        .backend
        .room_post_seal_key(channel.0)
        .await
        .expect("a keyed member seals");
    assert_eq!(seal, RoomPostSeal::Community { generation });
    assert_eq!(
        *base,
        fauna_core::group_content::room_post_base_key(&gen_key),
        "the post kind's key — a room's posts and messages never share one"
    );
    assert_ne!(
        *base,
        fauna_core::group_content::group_content_key(
            &gen_key,
            fauna_core::group_content::ROOM_MESSAGE_CONTENT_KIND
        )
    );
    assert_eq!(
        *reader
            .backend
            .room_post_base_key(channel.0, seal)
            .await
            .expect("a member holding the generation's wrap opens"),
        *base
    );
    assert!(
        reader
            .backend
            .room_post_base_key(
                channel.0,
                RoomPostSeal::Community {
                    generation: [0x01; 32]
                }
            )
            .await
            .is_err(),
        "a generation this member was never wrapped into stays sealed"
    );
}

/// A device with no room keys at all — no MLS group, no generation seams —
/// answers both questions with a refusal, never a key.
#[tokio::test]
async fn a_device_holding_no_room_keys_seals_and_opens_nothing() {
    let engine = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let backend = FaunaMlsBackend::new(
        engine.clone(),
        Arc::new(MockNest::default()),
        "loner",
        engine.identity_actor_id(),
    );
    assert!(backend.room_post_seal_key([0x44; 32]).await.is_err());
    assert!(
        backend
            .room_post_base_key(
                [0x44; 32],
                RoomPostSeal::Community {
                    generation: [0x9A; 32]
                }
            )
            .await
            .is_err()
    );
    assert!(
        backend
            .room_post_base_key([0x44; 32], RoomPostSeal::EndToEnd { epoch: 1 })
            .await
            .is_err()
    );
}

/// The **backfill** (`conversation-rooms.md` § The floor roster →
/// *End-to-end rooms*, the birth report's declared residue): a room whose
/// best-effort birth report failed (or a crash right after the first post)
/// never bootstraps again, so its home has no floor — and a 1:1 never gets one
/// at all, because it carries no membership or policy commit to ride.
///
/// The fixture stands in for "report lost" the only way a test can: the group
/// is bootstrapped, its birth report is drained, and the home is then scripted
/// to answer the read EMPTY-HANDED — which is exactly what a home that never
/// received a birth report says.
#[tokio::test]
async fn a_room_whose_birth_report_failed_backfills_its_floor_exactly_once() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob.identity_actor_id().0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let reporter = Arc::new(RecordingReporter::default());
    backend.set_room_roster_reporter(reporter.clone());
    let thread = fauna_mls_thread(
        ThreadId("dm-1".into()),
        vec![fauna_addr("bob", bob.identity_actor_id())],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("the first send bootstraps the 1:1");
    let channel_id = channel_id_from_hex(&backend.channel_binding_hex(&thread.thread_id).unwrap());

    // Drain the birth report: from here the fixture is a room whose home has
    // no floor, which is the state this backfill exists for.
    reporter.reports.lock().unwrap().clear();

    // Two answerless reads are scripted, so a second attempt would be visible
    // as a second call rather than degrading to the queue running dry.
    let reader = Arc::new(ScriptedReader::new(vec![None, None]));
    backend.set_room_roster_reader(reader.clone());

    backend.backfill_floor_roster(&channel_id).await;
    assert_eq!(reader.calls(), 1, "the backfill reads before it reports");
    {
        let reports = reporter.reports.lock().unwrap();
        assert_eq!(reports.len(), 1, "one backfill report");
        let report = &reports[0];
        assert_eq!(
            report.commit_seq, None,
            "a restore has no commit of its own to name — the same unpositioned \
             shape as the birth report, admitted by the home nest's bootstrap bound"
        );
        assert_eq!(
            report.policy_version, None,
            "a 1:1 carries no policy, so there is no version to report"
        );
        let mut members: Vec<(ActorId, Option<RoomRole>)> =
            report.members.iter().map(|m| (m.actor, m.role)).collect();
        members.sort_by_key(|(a, _)| a.0);
        let mut expected = vec![(alice_actor, None), (bob.identity_actor_id(), None)];
        expected.sort_by_key(|(a, _)| a.0);
        assert_eq!(members, expected, "both principals, role-less");
    }

    // ...and never again this session: the memo is spent, so the second pass
    // does not even read. A room that answers nothing costs one read per
    // session, not one per poll.
    backend.backfill_floor_roster(&channel_id).await;
    assert_eq!(reader.calls(), 1, "the second pass does not read again");
    assert_eq!(
        reporter.reports.lock().unwrap().len(),
        1,
        "exactly once, and never again"
    );
}

///  A read that FAILS is not a read that confirms the floor
/// empty — the backfill must not fold the two together the way the old
/// `Option<RoomFloor>` seam did, or a transient blip (a nest restart, a
/// dropped connection) sends an unpositioned report that rolls back a floor
/// this device simply failed to see. Mutating the fix back to folding
/// `Unavailable` into "no floor" must redden this: it would produce a report
/// on the first pass and never retry.
#[tokio::test]
async fn a_failed_backfill_read_reports_nothing_and_retries_next_poll() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob.identity_actor_id().0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let reporter = Arc::new(RecordingReporter::default());
    backend.set_room_roster_reporter(reporter.clone());
    let thread = fauna_mls_thread(
        ThreadId("dm-1".into()),
        vec![fauna_addr("bob", bob.identity_actor_id())],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("the first send bootstraps the 1:1");
    let channel_id = channel_id_from_hex(&backend.channel_binding_hex(&thread.thread_id).unwrap());
    reporter.reports.lock().unwrap().clear();

    // The first read fails outright — a transport fault, not a clean "no
    // floor" — and the second confirms the floor genuinely absent.
    let reader = Arc::new(ScriptedReader::new_scripted(vec![
        ScriptedRosterAnswer::Unavailable,
        ScriptedRosterAnswer::NoFloor,
    ]));
    backend.set_room_roster_reader(reader.clone());

    backend.backfill_floor_roster(&channel_id).await;
    assert_eq!(reader.calls(), 1, "it read once");
    assert!(
        reporter.reports.lock().unwrap().is_empty(),
        "a read that failed must not be taken as a confirmed-empty floor"
    );

    // The attempt was left unspent, so this pass reads again rather than
    // treating the first (failed) read as its one attempt for the session.
    backend.backfill_floor_roster(&channel_id).await;
    assert_eq!(
        reader.calls(),
        2,
        "the failed read did not spend the attempt"
    );
    assert_eq!(
        reporter.reports.lock().unwrap().len(),
        1,
        "the retry's confirmed-empty answer reports exactly once"
    );
}

/// The ordering trap this backfill is gated on: its report names no log
/// position, and an unpositioned report is taken WHOLESALE by the home nest
/// (the supersede check runs only for a report that names one). So a room
/// whose floor already exists must never be reported at — and the read is what
/// tells the two apart: `read_roster` answers `Some` only when a floor exists
/// AND this device is on it, which is precisely the case that must not be
/// overwritten.
#[tokio::test]
async fn a_room_whose_floor_already_answers_is_never_rolled_back_by_a_backfill() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob.identity_actor_id().0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let reporter = Arc::new(RecordingReporter::default());
    backend.set_room_roster_reporter(reporter.clone());
    let thread = fauna_mls_thread(
        ThreadId("dm-1".into()),
        vec![fauna_addr("bob", bob.identity_actor_id())],
    );
    backend
        .send(
            &thread,
            &ComposeState {
                body_draft: "hello".into(),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("the first send bootstraps the 1:1");
    let channel_id = channel_id_from_hex(&backend.channel_binding_hex(&thread.thread_id).unwrap());
    reporter.reports.lock().unwrap().clear();

    // The home answers with a floor — a later one than this device could
    // reconstruct, and one it is on.
    let reader = Arc::new(ScriptedReader::new(vec![Some(vec![
        known(alice_actor, Some("alice")),
        known(bob.identity_actor_id(), Some("bob")),
    ])]));
    backend.set_room_roster_reader(reader.clone());

    backend.backfill_floor_roster(&channel_id).await;
    assert_eq!(reader.calls(), 1, "it reads to find out");
    assert!(
        reporter.reports.lock().unwrap().is_empty(),
        "a floor that answers is a floor that must not be replaced: an \
         unpositioned report would be taken wholesale and roll it back"
    );
}

/// A group-LESS channel is [`FaunaMlsBackend::tend_community_room`]'s case, and
/// this pass returns before its read — the two are mirrors and each declines
/// the other's channels. Without this bound the backfill would report a roster
/// for a community room, whose floor is its own membership authority and whose
/// door refuses a member report outright.
#[tokio::test]
async fn the_backfill_declines_a_channel_this_device_holds_no_group_for() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let nest = Arc::new(MockNest::default());
    let alice_actor = alice.identity_actor_id();
    let backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let reporter = Arc::new(RecordingReporter::default());
    backend.set_room_roster_reporter(reporter.clone());
    let reader = Arc::new(ScriptedReader::new(vec![None]));
    backend.set_room_roster_reader(reader.clone());

    // A channel this device never bootstrapped a group for.
    let channel_id = channel_id_from_hex(&"ab".repeat(32));
    backend.backfill_floor_roster(&channel_id).await;

    assert_eq!(reader.calls(), 0, "it does not even read");
    assert!(
        reporter.reports.lock().unwrap().is_empty(),
        "a group-less channel is tend_community_room's case, not this one"
    );
}

// ── The shared receive loop's own bounds ──────────────────────────────────────
//
// `fetch_open_cache_attachments` walks a message's SEALED attachment list —
// written by its author alone, so no send-time check can bound it — fetching,
// opening and caching each entry for every recipient. Each case below pins one
// of the reader's own bounds, all taken from `fauna_core::attachment_limits`
// (`conversation-rooms.md` § The home nest → *Attachment bytes*).

/// A resolved attachment over `bytes`, handled by their plaintext BLAKE3.
fn resolved_bytes(bytes: Vec<u8>, filename: &str) -> ResolvedAttachment {
    ResolvedAttachment {
        blob_hash: hex::encode(blake3::hash(&bytes).as_bytes()),
        filename: filename.into(),
        mime_type: "application/octet-stream".into(),
        is_image: false,
        bytes,
    }
}

/// Bob ingests Alice's Welcome and polls once, asserting every message Alice
/// sent arrived; returns his manager and the thread.
async fn bob_ingests_and_polls(
    nest: &Arc<MockNest>,
    bob: Arc<MlsEngine>,
    bob_actor: ActorId,
    expected_ingested: usize,
) -> (Arc<ConversationsManager>, ThreadId) {
    let bob_manager = ConversationsManager::new();
    let bob_backend = Arc::new(FaunaMlsBackend::new(bob, nest.clone(), "bob", bob_actor));
    bob_manager.register_backend(bob_backend.clone());
    let welcome = nest.welcomes()[0].clone();
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(
        ingested, expected_ingested,
        "every message Alice sent was ingested"
    );
    (bob_manager, thread_id)
}

/// Alice bootstraps a 1:1 with Bob with one message carrying `attachments`, and
/// Bob receives it. Returns Bob's manager, the thread, and how many blob reads
/// Bob's receive made — every `blob_put` precedes his first read, so his reads
/// are exactly the calls `blob_homes` gained after Alice's send.
async fn bob_receives_one_message_carrying(
    attachments: &[ResolvedAttachment],
) -> (Arc<ConversationsManager>, ThreadId, usize) {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    let compose = ComposeState {
        body_draft: "see attached".into(),
        ..Default::default()
    };
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &compose,
            attachments,
        )
        .await
        .expect("alice bootstrap + send with attachments");
    let puts = nest.blob_homes.lock().unwrap().len();
    let (bob_manager, thread_id) = bob_ingests_and_polls(&nest, bob, bob_actor, 1).await;
    let gets = nest.blob_homes.lock().unwrap().len() - puts;
    (bob_manager, thread_id, gets)
}

/// The declared sizes of every attachment block the thread rendered, in order.
fn rendered_attachment_sizes(manager: &ConversationsManager, thread_id: ThreadId) -> Vec<u64> {
    let detail = manager.thread_detail(thread_id).expect("thread");
    detail
        .messages
        .iter()
        .flat_map(|m| {
            attachment_blocks(&m.document)
                .into_iter()
                .map(|a| a.size_bytes)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A message naming more attachments than one record may pin is walked only to
/// the cap: Bob fetches and renders the first `MAX_ATTACHMENTS_PER_RECORD`, in
/// the author's order, and never asks the nest for the rest. Without the bound
/// every entry costs every recipient one GET, however many the author names.
#[tokio::test]
async fn a_message_naming_more_attachments_than_a_record_may_pin_is_walked_only_to_the_cap() {
    let cap = fauna_core::attachment_limits::MAX_ATTACHMENTS_PER_RECORD;
    let attachments: Vec<ResolvedAttachment> = (0..=cap)
        .map(|i| resolved_bytes(format!("attachment number {i:03}").into_bytes(), "f.bin"))
        .collect();
    let (bob_manager, thread_id, gets) = bob_receives_one_message_carrying(&attachments).await;
    assert_eq!(
        gets, cap,
        "one blob read per walked entry, and none past the cap"
    );
    assert_eq!(
        rendered_attachment_sizes(&bob_manager, thread_id).len(),
        cap,
        "the first `cap` entries render"
    );
    assert!(
        bob_manager
            .attachment_bytes(attachments[cap].blob_hash.clone())
            .is_none(),
        "the entry past the cap was never fetched into the cache"
    );
}

/// An attachment whose declared size is over the inline blob limit is refused
/// before a byte is fetched: no legitimate sealed blob that large ever came to
/// rest, and the declared size is the author's signed word, so it is a free
/// reason not to ask. Its bubble renders without it.
#[tokio::test]
async fn an_attachment_declared_over_the_inline_blob_limit_is_never_fetched() {
    let limit = fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT;
    let big = resolved_bytes(vec![7u8; limit + 1], "big.bin");
    let (bob_manager, thread_id, gets) =
        bob_receives_one_message_carrying(std::slice::from_ref(&big)).await;
    assert_eq!(
        gets, 0,
        "an over-limit declared size is refused before the GET"
    );
    assert!(
        rendered_attachment_sizes(&bob_manager, thread_id).is_empty(),
        "nothing renders for it"
    );
    assert!(
        bob_manager
            .attachment_bytes(big.blob_hash.clone())
            .is_none(),
        "nothing was cached under its handle"
    );
}

/// A sealed blob larger than the inline blob limit is never opened, even when
/// its declared plaintext size is within it: the nest's own upload door refuses
/// that many bytes, so a nest serving them is not serving an attachment. The
/// plaintext here sits exactly at the limit, and the seal's framing pushes the
/// sealed blob past it — a body only a door without the limit (this mock)
/// would take.
#[tokio::test]
async fn a_sealed_blob_larger_than_the_inline_blob_limit_is_never_opened() {
    let limit = fauna_core::attachment_limits::INLINE_BLOB_BODY_LIMIT;
    let at_limit = resolved_bytes(vec![9u8; limit], "at-limit.bin");
    let (bob_manager, thread_id, gets) =
        bob_receives_one_message_carrying(std::slice::from_ref(&at_limit)).await;
    assert_eq!(gets, 1, "a declared size at the limit is fetched");
    assert!(
        rendered_attachment_sizes(&bob_manager, thread_id).is_empty(),
        "an over-limit sealed blob renders nothing"
    );
    assert!(
        bob_manager
            .attachment_bytes(at_limit.blob_hash.clone())
            .is_none(),
        "the over-limit sealed blob was never opened into the cache"
    );
}

/// An attachment whose opened plaintext is not the size its reference declares
/// is a failed open: skipped whole, nothing cached, no bubble pointing at it.
/// The declared size is what every app shows beside the file, so a reference
/// lying about it is not rendered. Crafted by hand, because the real send path
/// stamps the true length.
#[tokio::test]
async fn an_attachment_whose_opened_length_is_not_its_declared_size_is_skipped() {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice.clone(), nest.clone(), "alice", alice_actor);
    let hello = ComposeState {
        body_draft: "hello".into(),
        ..Default::default()
    };
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &hello,
            &[],
        )
        .await
        .expect("alice bootstrap");
    let channel = alice_backend.bound_channels()[0];

    let png: &[u8] = b"\x89PNG\r\n\x1a\nthe-declared-size-lies";
    let sealed = alice
        .seal_conversation_blob(&channel, png)
        .expect("seal the attachment");
    let sealed_cid_hex = hex::encode(sealed.sealed_cid);
    nest.blob_put(
        channel.to_string(),
        None,
        sealed_cid_hex.clone(),
        sealed.sealed,
    )
    .await
    .expect("upload the sealed attachment");
    let lying = fauna_mls::types::ChannelAttachment {
        blob_hash: fauna_core::data::ContentHash::from_digest_raw(*blake3::hash(png).as_bytes()),
        sealed_cid: fauna_core::data::ContentHash::from_digest_raw(sealed.sealed_cid),
        filename: "pic.png".into(),
        mime_type: "image/png".into(),
        size_bytes: png.len() as u64 + 1,
        is_image: true,
        epoch: sealed.epoch,
    };
    let message = ChannelMessage {
        sender: alice_actor,
        sequence: 1_000,
        channel_epoch: 0,
        body: ChannelMessageBody::Attachments {
            body: "size lie".into(),
            attachments: vec![lying],
        },
        timestamp: Timestamp::now(),
    };
    let ciphertext = alice.encrypt(&channel, &message).expect("encrypt");
    let envelope = ChannelEnvelope::Application(ciphertext)
        .to_bytes()
        .expect("envelope");
    alice_backend
        .send_on_channel(&channel, envelope, None, vec![sealed_cid_hex])
        .await
        .expect("post the crafted message");

    let (bob_manager, thread_id) = bob_ingests_and_polls(&nest, bob, bob_actor, 2).await;
    assert!(
        rendered_attachment_sizes(&bob_manager, thread_id).is_empty(),
        "an attachment lying about its size renders nothing"
    );
    assert!(
        bob_manager
            .attachment_bytes(hex::encode(blake3::hash(png).as_bytes()))
            .is_none(),
        "nothing was cached under its handle"
    );
}

// ── The store is a bounded cache: an evicted attachment is fetched again ──
//
// `conversations.md` § Attachments → *Retention*. The reader bound above caps
// one message; the store's budget caps what the app holds. What the budget
// evicts on the FaunaMls rail is not lost: the receive loop remembered where the
// bytes rest (channel, sealed content address, opening key), a render-time miss
// marks the handle wanted, and the next receive cycle fetches + opens + caches
// it again — for both classes, since the opener is the only thing that differs.

/// Bob receives one message carrying `attachments` into a manager whose store
/// holds at most `budget` bytes. Returns his manager, backend, thread and the
/// nest (for counting blob reads).
async fn bob_receives_under_a_budget(
    attachments: &[ResolvedAttachment],
    budget: usize,
) -> (
    Arc<ConversationsManager>,
    Arc<FaunaMlsBackend>,
    ThreadId,
    Arc<MockNest>,
) {
    let alice = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let bob = Arc::new(MlsEngine::new_in_memory(ActorKeypair::generate()).unwrap());
    let alice_actor = alice.identity_actor_id();
    let bob_actor = bob.identity_actor_id();
    let nest = Arc::new(MockNest::default());
    nest.seed_keypackage(
        &hex::encode(bob_actor.0),
        bob.generate_key_packages_bytes(1).unwrap()[0].clone(),
    );
    let alice_backend = FaunaMlsBackend::new(alice, nest.clone(), "alice", alice_actor);
    alice_backend
        .send(
            &fauna_mls_thread(ThreadId("a-1".into()), vec![fauna_addr("bob", bob_actor)]),
            &ComposeState {
                body_draft: "see attached".into(),
                ..Default::default()
            },
            attachments,
        )
        .await
        .expect("alice bootstrap + send with attachments");
    let bob_manager = ConversationsManager::new();
    bob_manager.set_attachment_store_budget_for_test(budget);
    let bob_backend = Arc::new(FaunaMlsBackend::new(bob, nest.clone(), "bob", bob_actor));
    bob_manager.register_backend(bob_backend.clone());
    let welcome = nest.welcomes()[0].clone();
    let thread_id = ingest_welcome(
        &bob_backend,
        &bob_manager,
        &welcome.channel_hex,
        &welcome.welcome_bytes,
        "",
    )
    .await
    .expect("welcome ingest");
    let mut after_seq = 0i64;
    let ingested = poll_inbound_conv(
        &bob_backend,
        &bob_manager,
        &channel_id_from_hex(&welcome.channel_hex),
        &mut after_seq,
        0,
    )
    .await
    .expect("poll ok")
    .ingested;
    assert_eq!(ingested, 1, "the attachment message was ingested");
    (bob_manager, bob_backend, thread_id, nest)
}

#[tokio::test]
async fn an_attachment_the_budget_evicted_is_fetched_again_on_the_next_receive_cycle() {
    let attachments: Vec<ResolvedAttachment> = (b'a'..=b'c')
        .map(|c| resolved_bytes(vec![c; 40], "f.bin"))
        .collect();
    // Three 40-byte attachments under a 100-byte budget: the third insert
    // evicts the first (least recently read).
    let (bob_manager, bob_backend, thread_id, nest) =
        bob_receives_under_a_budget(&attachments, 100).await;
    assert_eq!(
        rendered_attachment_sizes(&bob_manager, thread_id).len(),
        3,
        "every attachment renders — the bubble is complete whatever the store holds"
    );
    assert!(bob_manager.attachment_store_resident_bytes() <= 100);
    let first = attachments[0].blob_hash.clone();
    assert!(
        bob_manager.attachment_bytes(first.clone()).is_none(),
        "the first attachment was evicted to make room for the third"
    );
    for att in &attachments[1..] {
        assert!(
            bob_manager
                .attachment_bytes(att.blob_hash.clone())
                .is_some(),
            "the two most recently cached stay resident"
        );
    }

    // That miss was a render asking for the bytes: the next receive cycle
    // fetches exactly that one blob again and caches it under its handle.
    let reads_before = nest.blob_homes.lock().unwrap().len();
    let refilled = refill_evicted_attachments(&bob_backend, &bob_manager).await;
    assert_eq!(refilled, 1, "one wanted attachment, one refill");
    assert_eq!(
        nest.blob_homes.lock().unwrap().len() - reads_before,
        1,
        "one blob read — the wanted one, nothing else re-fetched"
    );
    assert_eq!(
        bob_manager.attachment_bytes(first).as_deref(),
        Some(attachments[0].bytes.as_slice()),
        "the evicted bytes resolve again, verified against their handle"
    );
    assert!(
        bob_manager.attachment_store_resident_bytes() <= 100,
        "the refill respects the budget too — something else made room"
    );

    // Nothing wanted → nothing fetched.
    let reads_before = nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&bob_backend, &bob_manager).await,
        0
    );
    assert_eq!(nest.blob_homes.lock().unwrap().len(), reads_before);
}

#[tokio::test]
async fn a_room_attachment_the_budget_evicted_is_fetched_again_under_the_rooms_key() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2du8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x5du8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);
    bob.manager.set_attachment_store_budget_for_test(100);

    let attachments: Vec<ResolvedAttachment> = (b'p'..=b'q')
        .map(|c| resolved_bytes(vec![c; 60], "f.bin"))
        .collect();
    alice
        .backend
        .send(
            &fauna_mls_thread(alice.thread.clone(), vec![]),
            &ComposeState {
                body_draft: "two pictures".into(),
                ..Default::default()
            },
            &attachments,
        )
        .await
        .expect("a keyed seat can send attachments into its room");

    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let first = attachments[0].blob_hash.clone();
    assert!(
        bob.manager.attachment_bytes(first.clone()).is_none(),
        "two 60-byte attachments under a 100-byte budget: the first was evicted"
    );
    assert!(
        bob.manager
            .attachment_bytes(attachments[1].blob_hash.clone())
            .is_some()
    );

    let reads_before = nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&bob.backend, &bob.manager).await,
        1,
        "the community class refills through the same loop — only the opener differs"
    );
    assert_eq!(nest.blob_homes.lock().unwrap().len() - reads_before, 1);
    assert_eq!(
        bob.manager.attachment_bytes(first).as_deref(),
        Some(attachments[0].bytes.as_slice()),
        "opened again under the room's attachment content kind off the naming generation"
    );
}

/// A device restored from the `history/<ch>` replica starts with an empty
/// attachment store, and its poll resumes past the records that named the
/// restored attachments — the walk never comes back to them, so before the
/// coordinates rode the slice every restored attachment rendered declared for
/// good (`conversations.md` § Attachments → *Retention*). Now the first
/// render's miss is fetched exactly as an eviction refill is, and no poll runs
/// at all: nothing but the refill could have cached these bytes.
#[tokio::test]
async fn a_device_restored_from_a_history_slice_fetches_its_attachments_without_a_log_walk() {
    let attachments: Vec<ResolvedAttachment> = (b'x'..=b'y')
        .map(|c| resolved_bytes(vec![c; 40], "f.bin"))
        .collect();
    let (bob_manager, bob_backend, thread_id, nest) =
        bob_receives_under_a_budget(&attachments, 1024).await;
    let channel_hex = nest.welcomes()[0].channel_hex.clone();

    // Device one's replica capture — the one door both `history/<ch>` writers
    // take — through the at-rest bytes device two decodes.
    let captured = bob_manager
        .snapshot_channel_slice(&thread_id, &channel_hex, 1)
        .expect("a bound thread snapshots");
    assert_eq!(
        captured.attachment_coordinates.len(),
        2,
        "the slice records where both attachments rest"
    );
    let restored = ChannelHistorySlice::from_bytes(&captured.to_bytes().unwrap()).unwrap();

    // Device two: the same leaf (the `provider` replica restores the engine)
    // and a fresh manager, re-seeded from the slice alone.
    let engine = bob_backend.engine();
    let device_two = ConversationsManager::new();
    let backend_two = Arc::new(FaunaMlsBackend::new(
        engine.clone(),
        nest.clone(),
        "bob",
        engine.identity_actor_id(),
    ));
    device_two.register_backend(backend_two.clone());
    let thread_two = device_two.restore_channel_slice(&restored);
    backend_two.bind_channel(thread_two.clone(), channel_id_from_hex(&channel_hex));
    assert_eq!(
        rendered_attachment_sizes(&device_two, thread_two).len(),
        2,
        "the restored bubble names both attachments"
    );

    for att in &attachments {
        assert!(
            device_two.attachment_bytes(att.blob_hash.clone()).is_none(),
            "nothing is cached on the restored device yet — each render misses"
        );
    }
    let reads_before = nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&backend_two, &device_two).await,
        2,
        "both first-render misses are fetched from where the slice says they rest"
    );
    assert_eq!(nest.blob_homes.lock().unwrap().len() - reads_before, 2);
    for att in &attachments {
        assert_eq!(
            device_two
                .attachment_bytes(att.blob_hash.clone())
                .as_deref(),
            Some(att.bytes.as_slice()),
            "opened under the restored engine and verified against the handle"
        );
    }
}

// ── The sender's own attachments ────────────────────────────────────────────
//
// A sender never walks its own record back: the end-to-end class cannot open
// it (MLS never decrypts one's own application message), and the community
// class — which can — skips it on the poll's `thread_holds_message` dedup, because the
// Sent copy `ConversationsManager::send` appended already holds the record's
// `conv:<channel>:<seq>` id. So no receive loop ever learns where the sender's
// own sealed blobs rest; the send is the one moment that knows. These cases pin
// that it tells the manager — without it, an own attachment the budget evicted
// on the sending device, or restored from `history/<ch>` on another, rendered
// declared while its bytes sat on the home nest.

/// Stage `attachments` on `thread`'s composer and send them through the
/// manager — the path every app's Send takes, which appends the sender's own
/// copy.
async fn send_staged_attachments(
    manager: &ConversationsManager,
    thread: &ThreadId,
    attachments: &[ResolvedAttachment],
) {
    for att in attachments {
        let staged = manager.add_attachment(
            thread.clone(),
            att.filename.clone(),
            att.mime_type.clone(),
            att.bytes.clone(),
        );
        assert_eq!(staged, att.blob_hash, "staged under its plaintext handle");
    }
    manager.set_compose_body(thread.clone(), "see attached".into());
    manager.send(thread.clone()).await.expect("send ok");
}

/// Cache 40 bytes the sender never sent — an unrelated read that pushes a
/// 100-byte store holding two sent 40-byte attachments over its budget.
fn read_something_unrelated(manager: &ConversationsManager) {
    let unrelated = resolved_bytes(vec![b'z'; 40], "unrelated.bin");
    manager.cache_attachment_bytes(unrelated.blob_hash, unrelated.bytes);
}

#[tokio::test]
async fn an_attachment_the_sender_sent_is_fetched_again_after_its_own_device_evicts_it() {
    let g = governed_room();
    let alice = alice_seat(&g);
    alice.manager.set_attachment_store_budget_for_test(100);
    let attachments: Vec<ResolvedAttachment> = (b'a'..=b'b')
        .map(|c| resolved_bytes(vec![c; 40], "f.bin"))
        .collect();
    send_staged_attachments(&alice.manager, &alice.tid, &attachments).await;
    assert_eq!(
        rendered_attachment_sizes(&alice.manager, alice.tid.clone()),
        vec![40, 40],
        "the sender's own copy names both attachments"
    );

    read_something_unrelated(&alice.manager);
    let first = attachments[0].blob_hash.clone();
    assert!(
        alice.manager.attachment_bytes(first.clone()).is_none(),
        "the sent draft no longer pins its bytes, so the budget evicted the older one"
    );
    assert!(
        alice
            .manager
            .attachment_bytes(attachments[1].blob_hash.clone())
            .is_some()
    );

    let reads_before = g.nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&alice.backend, &alice.manager).await,
        1,
        "the send remembered where its own sealed blob rests"
    );
    assert_eq!(g.nest.blob_homes.lock().unwrap().len() - reads_before, 1);
    assert_eq!(
        alice.manager.attachment_bytes(first).as_deref(),
        Some(attachments[0].bytes.as_slice()),
        "opened under the epoch the send sealed at, verified against the handle"
    );
}

#[tokio::test]
async fn a_room_attachment_the_sender_sent_is_fetched_again_though_its_own_poll_skips_the_record() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x2eu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    alice.hold(&GenerationKey::mint(), [0x5eu8; 32]);
    alice.manager.set_attachment_store_budget_for_test(100);
    let attachments: Vec<ResolvedAttachment> = (b'p'..=b'q')
        .map(|c| resolved_bytes(vec![c; 40], "f.bin"))
        .collect();
    send_staged_attachments(&alice.manager, &alice.thread, &attachments).await;

    // The sending device's own poll reaches the record it just sent. It holds
    // the generation key and could open it, but the Sent copy already holds
    // the record's id, so the walk skips it — and with it the fetch that would
    // have remembered where the blobs rest.
    let mut after_seq = 0i64;
    assert_eq!(
        alice.poll(&channel, &mut after_seq).await,
        0,
        "the poll skips the record the Sent copy already holds"
    );
    assert_eq!(after_seq, 1, "the walk did reach it");

    read_something_unrelated(&alice.manager);
    let first = attachments[0].blob_hash.clone();
    assert!(
        alice.manager.attachment_bytes(first.clone()).is_none(),
        "the budget evicted the older sent attachment"
    );

    let reads_before = nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&alice.backend, &alice.manager).await,
        1,
        "the send remembered the room generation it sealed under — the poll never could"
    );
    assert_eq!(nest.blob_homes.lock().unwrap().len() - reads_before, 1);
    assert_eq!(
        alice.manager.attachment_bytes(first).as_deref(),
        Some(attachments[0].bytes.as_slice()),
        "opened under the room's attachment content kind off the tip the send sealed under"
    );
}

/// The sender's other device, restored from the `history/<ch>` replica,
/// fetches the attachments the first device *sent*: the slice carries the
/// coordinates the send remembered, since no walk on either device learns them.
#[tokio::test]
async fn a_device_restored_from_a_history_slice_fetches_the_attachments_its_other_device_sent() {
    let g = governed_room();
    let alice = alice_seat(&g);
    let attachments: Vec<ResolvedAttachment> = (b'x'..=b'y')
        .map(|c| resolved_bytes(vec![c; 40], "f.bin"))
        .collect();
    send_staged_attachments(&alice.manager, &alice.tid, &attachments).await;
    let channel_hex = g.channel_id.to_string();

    let captured = alice
        .manager
        .snapshot_channel_slice(&alice.tid, &channel_hex, 1)
        .expect("a bound thread snapshots");
    assert_eq!(
        captured.attachment_coordinates.len(),
        2,
        "the slice records where both sent attachments rest"
    );
    let restored = ChannelHistorySlice::from_bytes(&captured.to_bytes().unwrap()).unwrap();

    // Device two: the same leaf (the `provider` replica restores the engine)
    // and a fresh manager, re-seeded from the slice alone.
    let device_two = ConversationsManager::new();
    let backend_two = Arc::new(FaunaMlsBackend::new(
        g.alice.clone(),
        g.nest.clone(),
        "alice",
        g.alice.identity_actor_id(),
    ));
    device_two.register_backend(backend_two.clone());
    let thread_two = device_two.restore_channel_slice(&restored);
    backend_two.bind_channel(thread_two.clone(), g.channel_id);
    assert_eq!(
        rendered_attachment_sizes(&device_two, thread_two),
        vec![40, 40],
        "the restored own bubble names both attachments"
    );

    for att in &attachments {
        assert!(
            device_two.attachment_bytes(att.blob_hash.clone()).is_none(),
            "nothing is cached on the restored device yet — each render misses"
        );
    }
    let reads_before = g.nest.blob_homes.lock().unwrap().len();
    assert_eq!(
        refill_evicted_attachments(&backend_two, &device_two).await,
        2,
        "both sent attachments are fetched from where the slice says they rest"
    );
    assert_eq!(g.nest.blob_homes.lock().unwrap().len() - reads_before, 2);
    for att in &attachments {
        assert_eq!(
            device_two
                .attachment_bytes(att.blob_hash.clone())
                .as_deref(),
            Some(att.bytes.as_slice()),
            "opened under the sending epoch and verified against the handle"
        );
    }
}

/// A community sender's own copy carries the record's plane identity. The
/// sender *can* open its own room record — it holds the generation key like
/// every other member — but its poll never ingests it: the walk skips a record
/// whose id the Sent copy already holds. So, exactly as on the end-to-end
/// class, the send is the only moment the sending device can learn the
/// identity (`account-sync-plane.md` § Built — T1's reporting half → *The
/// identity*: derived at ingest and at send, from the sealed envelope).
#[tokio::test]
async fn a_community_senders_own_copy_carries_the_records_plane_ref() {
    let nest = Arc::new(MockNest::default());
    let channel = ChannelId([0x3fu8; 32]);
    let alice = room_seat(&nest, channel, "alice", 1);
    let bob = room_seat(&nest, channel, "bob", 2);
    let gen_key = GenerationKey::mint();
    let generation_id = [0x6fu8; 32];
    alice.hold(&gen_key, generation_id);
    bob.hold(&gen_key, generation_id);

    alice
        .manager
        .set_compose_body(alice.thread.clone(), "the square is open".into());
    alice
        .manager
        .send(alice.thread.clone())
        .await
        .expect("a keyed seat can send into its room");

    let own = alice
        .manager
        .thread_detail(alice.thread.clone())
        .expect("thread")
        .messages
        .pop()
        .expect("the Sent copy");
    let plane_ref = own
        .plane_ref
        .expect("the sender's own copy carries the record's plane identity");

    // The receiver derives the same identity from the same filed bytes — one
    // record, one name, whoever holds it.
    let mut after_seq = 0i64;
    assert_eq!(bob.poll(&channel, &mut after_seq).await, 1);
    let theirs = bob
        .manager
        .thread_detail(bob.thread.clone())
        .expect("thread")
        .messages
        .pop()
        .expect("the received copy");
    assert_eq!(
        theirs.plane_ref.as_ref(),
        Some(&plane_ref),
        "sender and receiver name the same record on the same scope"
    );
}

// ── The private contact overlay's succession fold rides the witness verdict ──

/// Records every contact-overlay fold the manager asks its seam for.
#[derive(Default)]
struct RecordingFolds(Mutex<Vec<(String, String)>>);

impl RecordingFolds {
    fn asked(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().clone()
    }
}

impl fauna_conversations::backend::ContactOverlayFolds for RecordingFolds {
    fn fold(&self, predecessor_hex: &str, successor_hex: &str) {
        self.0
            .lock()
            .unwrap()
            .push((predecessor_hex.to_string(), successor_hex.to_string()));
    }
}

/// Register a recording fold seam on `manager` and load a nickname on
/// `person` — the member's own overlay on the identity about to succeed.
fn overlay_on(manager: &ConversationsManager, person: &ActorId) -> Arc<RecordingFolds> {
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    let folds = Arc::new(RecordingFolds::default());
    let generation = manager.register_contact_overlays(Some(folds.clone()));
    let overlay = ContactOverlay {
        nickname: Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some("Mum".into()),
        },
        ..Default::default()
    };
    assert!(manager.apply_contact_overlays(
        generation,
        [(person.to_hex(), overlay)].into_iter().collect()
    ));
    folds
}

/// **The overlay fold inherits the harvest wait** (`identity-succession.md`
/// § The succession statement → *the harvest wait*, its *Who waits* sentences;
/// `contacts.md` § The private overlay). The fold rides the verdict the
/// session's own witness gives — never a second, un-armed verify — so a
/// statement decoded BEFORE this session's harvest of the peer has settled
/// folds NO overlay; the sweep's settle re-drives it, and then the honest,
/// un-demoted head folds it. The witness's own wait is pinned with the real
/// `ChainWitness` (`fauna-client-recovery`'s
/// `witness.rs::a_statement_that_beats_the_sessions_harvest_waits_for_it`);
/// `SwitchWitness` stands in for it here, as in the settle test above.
#[tokio::test]
async fn a_statement_that_beats_the_harvest_folds_no_overlay_until_the_settle() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        signed,
        head,
        successor,
        successor_engine,
        ..
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let folds = overlay_on(&bob_manager, &alice_actor);
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(bob_backend.succession_statement_counts().parked, 1);
    assert!(
        folds.asked().is_empty(),
        "a statement awaiting this session's harvest folds no overlay"
    );

    witness.arm(head);
    settle_parked_successions(&bob_backend, &bob_manager, &alice_actor).await;
    assert_eq!(
        folds.asked(),
        vec![(alice_actor.to_hex(), successor.to_hex())],
        "the settle's re-drive verifies it, and the verdict folds the overlay"
    );
}

/// The demoted arm of the same pin: the harvest read the rotation, so the
/// settle re-drives the statement into the walk, which refuses it — and a
/// refused statement folds nothing, then or later.
#[tokio::test]
async fn a_statement_the_settle_sends_to_a_refusing_walk_folds_no_overlay() {
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        signed,
        successor_engine,
        ..
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let folds = overlay_on(&bob_manager, &alice_actor);
    // Never armed: every verify refuses, as the walk does a retired kit.
    let witness = Arc::new(SwitchWitness::default());
    bob_backend.set_succession_witness(witness.clone());

    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    settle_parked_successions(&bob_backend, &bob_manager, &alice_actor).await;
    assert!(witness.calls().ends_with(&["settled", "verify"]));
    assert!(
        folds.asked().is_empty(),
        "a refused statement folds nothing: {:?}",
        folds.asked()
    );
}

/// **The no-row case** (`identity-succession.md` § the harvest wait, the
/// ruling on a store keyed by the person): a statement whose succession
/// re-points no row here folds nothing on that delivery, even verified — the
/// fold rides the re-point, never the bare signature. When the ceremony does
/// reach this group, its verified re-point folds the overlay; and the verdict
/// then keeps folding at every projection load, so an item a sibling device
/// re-creates under the old identity afterwards is folded with no statement.
#[tokio::test]
async fn a_statement_that_repoints_no_row_folds_nothing_and_the_next_verdict_does() {
    use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp};
    let SuccessionSetup {
        alice,
        channel_id,
        nest,
        bob_manager,
        bob_backend,
        signed,
        head,
        successor,
        successor_engine,
        ..
    } = succession_receive_setup();
    let alice_actor = alice.identity_actor_id();
    let folds = overlay_on(&bob_manager, &alice_actor);
    bob_backend.set_succession_witness(Arc::new(HeadWitness(head)));

    // The statement, replayed into a group the ceremony never reached.
    post_statement(&alice, &channel_id, &nest, &signed).await;
    let mut after_seq = 0i64;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    assert_eq!(
        bob_backend.succession_statement_counts().not_in_this_group,
        1
    );
    assert!(folds.asked().is_empty(), "no re-point, no fold");

    // The ceremony reaches this group: the next verified verdict folds it.
    run_add_successor(&alice, &successor_engine, &channel_id, &nest).await;
    post_statement(&alice, &channel_id, &nest, &signed).await;
    run_remove_old(&successor_engine, &channel_id, &nest, &alice_actor).await;
    poll_inbound_conv(&bob_backend, &bob_manager, &channel_id, &mut after_seq, 0)
        .await
        .expect("poll ok");
    let pair = (alice_actor.to_hex(), successor.to_hex());
    assert_eq!(folds.asked(), vec![pair.clone()]);

    // A straggler item under the old identity, loaded after the fold.
    let generation = bob_manager.contact_overlays_generation();
    let notes = ContactOverlay {
        notes: Register {
            stamp: Stamp::new(9, [2; 32]),
            value: Some("moved house".into()),
        },
        ..Default::default()
    };
    bob_manager.apply_contact_overlays(
        generation,
        [(alice_actor.to_hex(), notes)].into_iter().collect(),
    );
    assert_eq!(
        folds.asked(),
        vec![pair.clone(), pair],
        "the projection-load reconcile folds it from the verdict already given"
    );
}
