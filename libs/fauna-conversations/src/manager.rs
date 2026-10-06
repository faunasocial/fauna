use crate::address::{Rail, TypedAddress, try_parse_typed_address};
use crate::backend::{
    BackendError, LinkPreviewResolution, LinkPreviewRpc, MailFeed, RailBackend, RailInboundMessage,
    ResolveResult, ResolvedAttachment, RoomPolicyEdit,
};
use crate::compose::{
    AddParticipantState, AttachmentDraft, ComposeState, RecipientPickerState, ResolveState,
    SendState,
};
use crate::contacts::ContactsCache;
use crate::eviction::CrossGroupEviction;
use crate::index_sink::{IndexableKind, IndexableMessage, MessageIndexObserver};
use crate::keying::{ThreadKey, key_for_inbound, normalize_subject};
use crate::mail_read::MailReadSync;
use crate::message::{
    AttachmentSnapshot, BodyFormat, DeleteClaim, MessageBadges, MessageId, MessageSnapshot,
};
use crate::observer::SnapshotObserver;
use crate::reactions::{StampedReactionEvent, fold_reactions};
use crate::snapshot::{
    ConversationsSnapshot, SortOrder, ThreadDetail, ThreadStateFacts, ThreadSummary,
};
use crate::store::history::ChannelHistorySlice;
use crate::store::threads::SentCopyOutcome;
use crate::store::{
    AttachmentCoordinates, AttachmentRead, AttachmentStore, DraftStore, MailRecordCoordinates,
    SealedBlobCoordinates, ThreadStore,
};
use crate::thread::{ThreadFlavor, ThreadId};
use fauna_client_moderation::{DetectionLabel, LocalDetectionStore};
use fauna_core::identity::ActorId;
use fauna_core::localized::LocalizedText;
use fauna_core::render::{PreviewState, resolve_link_preview_cached};
use fauna_mls::types::ReactionOp;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

/// Why a `conversations_accept_recipient` test-agent command declined, spelled
/// **once** for every app that has to refuse it.
///
/// [`ConversationsManager::accept_current_recipient_chip`] returns `false` when
/// the active picker had nothing it could turn into a chip. Convention 11
/// (`e2e-conventions.md`) makes that a *loud* refusal rather than a green ack:
/// before the arms were fixed, a commit that landed no chip acked green and
/// surfaced ~5 s later as the action layer's generic "chip not added", naming
/// neither the command nor the reason.
///
/// It lives here, beside the method whose `false` it explains, so the apps
/// cannot drift apart in wording — the failure text is the only thing a test
/// reads, and per-app phrasings would make one cross-app pin unassertable.
/// Rust consumers (tui, linux) use this constant directly; web's TypeScript arm
/// and the still-owed windows/apple arms carry the same sentence by hand, since
/// no FFI export is warranted for a debug-only string.
pub const ACCEPT_RECIPIENT_NO_CHIP_REASON: &str = "nothing committed — the active picker had no resolvable recipient \
     (empty input, or an address that resolved to no chip)";

/// [`ConversationsManager`]'s read-position seam and the generation it was
/// registered under ([`ConversationsManager::set_read_positions`]).
#[derive(Default)]
struct ReadPositionsSlot {
    generation: u64,
    seam: Option<Arc<dyn crate::backend::ReadPositions>>,
}

/// [`ConversationsManager`]'s contact-overlay seam, the generation it was
/// registered under ([`ConversationsManager::register_contact_overlays`]),
/// and the verified successions the fold reconciles against.
#[derive(Default)]
struct ContactOverlaySlot {
    generation: u64,
    folds: Option<Arc<dyn crate::backend::ContactOverlayFolds>>,
    /// Every witness-verified succession this session re-pointed a row for,
    /// predecessor → successor — the verdicts the fold's projection-load
    /// reconcile consumes (it verifies no statement itself). Bounded by the
    /// rosters, since only a verified re-point writes it.
    successions: HashMap<ActorId, ActorId>,
}

/// How many succession hops the fold follows to the terminal successor — the
/// verified walk's own ceiling (`fauna_client_recovery::succession`'s
/// `MAX_VERIFIED_CHAIN_LEN` bounds a real line far below this).
const MAX_FOLD_HOPS: usize = 64;

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct ConversationsManager {
    backends: RwLock<HashMap<Rail, Arc<dyn RailBackend>>>,
    threads: ThreadStore,
    drafts: DraftStore,
    /// The private contact overlay projection (`contacts.md` § The private
    /// overlay) — the names snapshot builders paint.
    contacts: Arc<ContactsCache>,
    /// The overlay's fold seam and the verdicts it folds on — generational,
    /// for [`Self::read_positions`]' reason: an identity change retires both,
    /// and a delivery under a retired generation is refused, so the outgoing
    /// account's overlays never paint the incoming account's names.
    contact_overlays: RwLock<ContactOverlaySlot>,
    observers: RwLock<Vec<Arc<dyn SnapshotObserver>>>,
    selected: RwLock<Option<ThreadId>>,
    /// The **selected message** inside the selected thread — the second half of
    /// `SearchNav::Mail`'s contract, *"open the thread **and** select this
    /// message in it"* (`docs/goal/ui/search.md` § State & data shape). Set only
    /// by [`Self::select_thread_and_message`]; every other selection path
    /// clears it, because a marker surviving a thread switch would point at a
    /// message the user did not ask for.
    ///
    /// Held as the raw id rather than an index: the thread's message list is
    /// re-projected on every emit (late parents, ingested replies), so an index
    /// would silently drift onto a different message. [`Self::thread_detail`]
    /// resolves it against the fetched window at read time.
    selected_message: RwLock<Option<MessageId>>,
    /// Whether the new-thread composer is the ACTIVE detail view. The draft
    /// itself lives in `drafts.new_thread()` and PERSISTS across thread switches
    /// (`docs/goal/ui/conversations.md` § Persistence — a half-written new
    /// message survives switching; only an explicit cancel or a successful send
    /// discards it). This flag only gates whether the snapshot surfaces that
    /// draft as the shown composer (`new_thread_compose`), so selecting a thread
    /// shows the thread while keeping the new-thread draft recoverable via `+`
    /// (`start_new_conversation` re-activates and restores it).
    new_thread_active: RwLock<bool>,
    sort: RwLock<SortOrder>,
    search: RwLock<Option<String>>,
    /// Per-message remote-image reveal set (render-model.md § D3). The single
    /// source of truth for "the user opted into loading this message's remote
    /// images": [`thread_detail`](Self::thread_detail) projects it onto each
    /// `MessageSnapshot.document` (`RemoteImage.revealed`), so no client keeps a
    /// `revealedRemote`/`remoteLoaded` dictionary of its own. **In-memory only**
    /// — the no-persistence posture (html-mail.md § Rendering) is unchanged; a
    /// reveal does not survive a restart, exactly as the per-app flag didn't.
    revealed_remote: RwLock<HashSet<MessageId>>,
    /// Community-room policy names this device holds no succession anchor for,
    /// per room — what [`Self::harvest_walk_actors`] adds to the thread
    /// rosters. Written by the room backend, which alone knows which names an
    /// anchored policy version carries; session memory, like the chains.
    policy_anchor_wants: RwLock<HashMap<fauna_mls::types::ChannelId, Vec<ActorId>>>,
    /// The room backend's parked floor delete records, per community room, **at
    /// rest** ([`ChannelHistorySlice::parked_floor_deletes`]): what
    /// [`Self::snapshot_channel_slice`] — the one door every `history/<ch>`
    /// writer takes — stamps, and [`Self::restore_channel_slice`] re-seeds. The
    /// live set is the backend's (it alone judges); the backend is this copy's
    /// one writer, mirroring every park and every pass's survivors, and drains
    /// a restored copy into its live set on its first pass over the room. Held
    /// here rather than asked of the backend because the manager cannot see
    /// its backends' memory, and a slice writer that had to remember to ask
    /// would one day forget (`conversation-rooms.md` § Implementation status
    /// today, residual *(d)*: the walk steps past a record before judging it,
    /// so a record parked at quit was, until this rode the replica, lost).
    parked_floor_deletes:
        RwLock<HashMap<fauna_mls::types::ChannelId, Vec<crate::store::history::ParkedFloorDelete>>>,
    /// Per-message reaction-event log (FaunaMls-only feature; conversations.md
    /// § Reactions & message delete). Source of truth for reactions; thread_detail
    /// FOLDS it onto each MessageSnapshot.reactions via fold_reactions. In-memory.
    reactions: RwLock<HashMap<MessageId, Vec<StampedReactionEvent>>>,
    /// Cooperative-delete tombstone set. thread_detail projects MessageSnapshot.deleted.
    deleted: RwLock<HashSet<MessageId>>,
    /// Inbound cooperative-delete CLAIMS: target MessageId → every delete posted
    /// against it, each judged on its own ([`DeleteClaim::admits`]). thread_detail
    /// honors a claim when the claimer IS the target's own sender (the
    /// forged-delete drop, conversations.md § Reactions & message delete —
    /// security floor enforced on ingest/projection, not just the action), or
    /// when the rail recorded a governing role for it at fold
    /// (conversation-rooms.md § Roles and authorization → *Delete any message —
    /// the mechanism*). Several per message, never one: a forged claim must not
    /// displace an honoured one. A claim for a not-yet-arrived target sits here
    /// until it shows up.
    delete_claims: RwLock<HashMap<MessageId, Vec<DeleteClaim>>>,
    add_participant: RwLock<Option<AddParticipantState>>,
    /// Plaintext attachment bytes keyed by `blob_hash` (lowercase-hex BLAKE3).
    /// The shared attachment store: `add_attachment` caches a picked file here;
    /// `send` re-resolves bytes from here into the wire message; the inbound
    /// parse caches each extracted attachment here; and the per-app render
    /// path reads bytes back via [`Self::attachment_bytes`]. Keeping the bytes
    /// off `ComposeState` / `MessageSnapshot` keeps observed snapshots light over
    /// UniFFI (`docs/goal/ui/conversations.md` § Attachments). **A bounded
    /// cache, never the home of the bytes** (`store::attachments`): at most
    /// `ATTACHMENT_STORE_BUDGET_BYTES` resident, least recently read evicted
    /// first, staged drafts pinned, an evicted FaunaMls attachment fetched again
    /// on the next receive cycle. In-memory only (mirrors the in-memory
    /// `DraftStore`), and deliberately so — the bytes rest on the nest.
    attachments: RwLock<AttachmentStore>,
    /// Runs the receive loop's next cycle now — installed by the native
    /// `ConversationsSession` (`set_attachment_refill_poke`), so a render that
    /// misses an evicted attachment is refilled promptly rather than at the
    /// backstop ticker. `None` on web (its JS loop ticks `pollConversations`)
    /// and on receive-only / test managers.
    attachment_refill_poke: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// The mail rail's read-state sync — the `\Seen` writes a read owes the
    /// nest and the flag-change cursor ([`crate::mail_read`]). Written by the
    /// read chokepoint ([`Self::notify`]) and the receive loop's mail sweep.
    mail_read: Mutex<MailReadSync>,
    /// Sends the owed `\Seen` writes now rather than at the next sweep —
    /// installed by the native `ConversationsSession`, the read twin of
    /// [`Self::attachment_refill_poke`]. `None` on web and on test managers,
    /// which drain the owed writes themselves.
    mail_read_poke: RwLock<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Resolved link-preview state keyed by URL (render-model.md § D4) — the
    /// conversations twin of `FeedManager::resolved_previews`. The shared producer emits a
    /// bubble's bare-url `RenderBlock::LinkPreview` block `Resolving`;
    /// [`resolve_link_preview`](Self::resolve_link_preview) records the terminal
    /// `Resolved`/`Failed` here, and [`thread_detail`](Self::thread_detail) folds it onto
    /// the matching block — BEFORE the D3 reveal walk, so the cached `revealed:false` can't
    /// clobber a just-revealed og:image. Identity-stable (a URL's preview doesn't depend on
    /// the thread), so it is never cleared. **In-memory only** (no-persistence posture
    /// unchanged).
    resolved_previews: RwLock<HashMap<String, PreviewState>>,
    /// The home-nest link-preview seam ([`LinkPreviewRpc`]), wired alongside the FaunaMls
    /// backend's [`ConversationsRpc`](crate::backend::ConversationsRpc) by the native + wasm
    /// glue. `None` on the receive-only / SMTP-only constructors — `resolve_link_preview`
    /// then degrades to no preview (the inline link still shows). Held at the manager (not a
    /// rail backend) because resolution is **rail-agnostic**: any rail's bubble can carry a
    /// bare-url `LinkPreview`, resolved by the user's own nest.
    link_preview_rpc: RwLock<Option<Arc<dyn LinkPreviewRpc>>>,
    /// The session-owned moderation **local-detection store**, installed by
    /// [`ConversationsSession::from_manager`] via [`Self::set_local_detection_store`]
    /// (`None` until then — receive-only / test managers, and web until slice 5).
    /// Every just-decrypted **incoming** social message flows through
    /// [`Self::ingest_inbound_to_thread`], which — when a store is installed —
    /// classifies the plaintext body (`fauna_core::text_heuristic::classify_text`)
    /// and `observe`s a spam label above the confidence gate, retaining the
    /// encrypted-mode social-content moderation signal the nest cannot produce
    /// (`docs/goal/behavior/moderation.md` § Layout & flow; `content-scoring.md`
    /// § two plaintext positions → client post-decrypt). The store is held behind
    /// an `Arc<Mutex<…>>` the **session** owns (per-user lifetime, dropped on
    /// logout), so its retained detections never outlive the session or leak across
    /// users; the queue reader ([`ConversationsSession::moderation_local_detections`])
    /// reads the same `Arc`. Held here (not on a rail backend) because
    /// classification is rail-agnostic — any inbound social bubble is a candidate —
    /// mirroring [`Self::link_preview_rpc`].
    local_detections: RwLock<Option<Arc<Mutex<LocalDetectionStore>>>>,
    /// The content-index sink, when this client builds a local search index
    /// (`crate::index_sink` — `content-index.md` § Ingest triggers, v1). Held
    /// beside `local_detections` for the same reason: indexing is rail-agnostic,
    /// so it hangs off the manager rather than a rail backend. `None` on a
    /// client with no builder (web, and any manager built for tests) — the
    /// observer seam is optional by construction.
    index_observer: RwLock<Option<Arc<dyn MessageIndexObserver>>>,
    /// The fauna-native rail's read-position seam
    /// ([`crate::backend::ReadPositions`]) — where [`Self::notify`]'s read of
    /// a native thread goes to become account state
    /// (`conversation-read-state.md` § The read-marker record). `None` until an
    /// app registers it at its account-store-ready edge, and for a host with
    /// no account runtime. Registration is generational: an identity change
    /// retires the seam ([`Self::clear_for_identity_change`]), and a delivery
    /// under a retired generation is refused, so the outgoing account's
    /// positions can never land on the incoming account's threads.
    read_positions: RwLock<ReadPositionsSlot>,
    /// The succession witness's durable peer anchors
    /// ([`crate::backend::PeerAnchorStore`]) — `None` until an app registers
    /// the account store at its store-ready edge, and again after an identity
    /// change retires the outgoing account's.
    peer_anchor_store: RwLock<Option<Arc<dyn crate::backend::PeerAnchorStore>>>,
    /// Where both inbound rails' sinks record a refused scheduling change on
    /// its way to the account plane ([`crate::refused_changes`]).
    refused_changes: Arc<crate::refused_changes::RefusedChangeInbox>,
    /// The page-level error projected onto [`ConversationsSnapshot::error`] —
    /// see that field for the scope (membership/label wire ops, never the
    /// compose send path) and for why it exists.
    page_error: RwLock<Option<LocalizedText>>,
    /// Whether this process is a **non-holder** of the conversations-engine
    /// role over this account's `mls_state.db` (`MlsError::ServedElsewhere`
    /// at engine construction — `account-data-plane.md` § Multi-instance
    /// concurrency, W5.6 (account-data-plane.md § Workstreams)). Deliberately **not** folded into [`Self::page_error`]:
    /// that slot is cleared on entry by every membership/label producer
    /// (`Self::clear_page_error`), which would let an unrelated gesture mask
    /// this standing condition — the same reasoning as tui's separate
    /// `ConversationsState::served_elsewhere` field. Set/cleared by
    /// [`Self::set_engine_served_elsewhere`] at the app's engine-construction
    /// site (never by a manager-internal producer), read by
    /// [`Self::engine_served_elsewhere`] with top precedence over
    /// `page_error`/send-failure in each app's `error-message` projection.
    engine_served_elsewhere: RwLock<bool>,
    /// The receive-loop generation most recently started over this manager —
    /// claimed by [`Self::begin_receive_loop`], one per
    /// `ConversationsSession::start_receive_loop`. A manager can outlive the
    /// session whose loop it serves (the FFI factory takes a caller-supplied
    /// manager), so a dead-rail report has to name WHICH loop died.
    receive_loop_generation: std::sync::atomic::AtomicU64,
    /// The newest receive-loop generation that died by panic, `0` for none —
    /// written by [`Self::mark_receive_stopped`], read by
    /// [`Self::receive_stopped`]. Atomics rather than a lock on purpose: the
    /// writer runs right after a panic that may have poisoned any lock the pass
    /// held, and the reader is every app's page-error projection, which must
    /// never itself panic on that poison. Deliberately not folded into
    /// [`Self::page_error`], for the reason [`Self::engine_served_elsewhere`]
    /// gives: a standing condition must not be cleared by an unrelated gesture.
    receive_stopped_generation: std::sync::atomic::AtomicU64,
    /// The `(mailbox, uid)`s of received mail records this process could not
    /// open under the account's complete standing key set and skipped past —
    /// written by the receive path ([`Self::note_unopenable_mail`]), retired
    /// when the same record later opens ([`Self::retire_unopenable_mail`]),
    /// read by every app's page-error projection as
    /// [`Self::unopenable_mail_count`]. Per process, like the mail cursors: a
    /// relaunch re-drains every feed and rebuilds it.
    unopenable_mail: RwLock<std::collections::BTreeSet<(MailFeed, u32)>>,
    /// The community-room invitations standing for this account, verified —
    /// projected onto [`ConversationsSnapshot::room_invitations`] and refreshed
    /// by [`Self::refresh_room_invitations`] on the receive loop's sweep.
    room_invitations: RwLock<Vec<crate::room::RoomInvitation>>,
    /// Which identity this manager currently serves, as a counter
    /// [`Self::clear_for_identity_change`] advances. It retires the writers that
    /// outlive the drop (`account-scoping.md` § The scoping taxonomy): a shell
    /// that keeps one manager across a switch captures it before a detached
    /// read ([`Self::identity_epoch`]) and hands it back with the result
    /// ([`Self::restore_drafts_at`]), which refuses a moved epoch. A lock, not
    /// an atomic: the clear holds it for writing across the bump and the wipe,
    /// a restore for reading across the check and the fill, so no restore can
    /// pass the check against the old identity and fill after the wipe.
    identity_epoch: RwLock<u64>,
}

/// Which recipient picker an async resolve speaks for. Two callers, two
/// meanings, and conflating them was a real bug waiting: the add-participant
/// overlay takes priority over the new-thread compose picker for anything the
/// *user* does, but a `__drafts` restore carries only the new-thread slot
/// (`docs/goal/ui/conversations.md` § Persistence), so the probe it owes must
/// never land on an overlay the user happens to have open when the late fetch
/// arrives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickerTarget {
    /// The add-participant overlay if open, else the new-thread compose picker
    /// — what a user gesture means (and what `accept_current_recipient_chip`
    /// commits against).
    Active,
    /// The new-thread compose picker specifically, overlay or no overlay.
    NewThread,
}

/// Crate-internal surface, deliberately OUTSIDE the `uniffi::export` block below:
/// these return verdicts the FFI has no use for, and adding them to the exported
/// impl would change the generated bindings' checksums for every app.
impl ConversationsManager {
    /// The list's active order — [`Self::snapshot`]'s `sort`, without building
    /// the filtered, sorted thread list around it (the e2e state publish reads
    /// it on every tick).
    pub fn sort_order(&self) -> SortOrder {
        *self.sort.read().unwrap()
    }

    /// The selected thread — [`Self::snapshot`]'s `selected_thread_id`, without
    /// building the thread list around it.
    pub fn selected_thread_id(&self) -> Option<ThreadId> {
        self.selected.read().unwrap().clone()
    }

    /// Claim a fresh receive-loop generation — called by
    /// `ConversationsSession::start_receive_loop` right before it spawns, and
    /// the start of what [`Self::receive_stopped`] reads. Starting a loop
    /// supersedes any earlier loop's death, so a banner a previous session left
    /// is retired here (with a notify, so the page re-renders without it).
    pub fn begin_receive_loop(&self) -> u64 {
        let was_stopped = self.receive_stopped();
        let generation = self
            .receive_loop_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if was_stopped {
            self.notify();
        }
        generation
    }

    /// Record that the receive loop of `generation` died by panic. The loop's
    /// supervisor is the only production caller; public so an app's tier_1
    /// test can stand the condition up without a panicking loop.
    ///
    /// `fetch_max`, so a superseded loop that dies after a newer one never
    /// overwrites the newer death. The notify is contained: the panic that
    /// stopped the loop may have poisoned an observer lock, and a second panic
    /// here would only bury the first — the atomic is already written, so the
    /// next render shows the banner either way.
    pub fn mark_receive_stopped(&self, generation: u64) {
        self.receive_stopped_generation
            .fetch_max(generation, std::sync::atomic::Ordering::SeqCst);
        if self.receive_stopped() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.notify()));
        }
    }

    /// Record that the receive path skipped `(mailbox, uid)` because it would
    /// not open ([`crate::backend::SkippedMailRecord`]). The mail receive
    /// drivers — native `poll_inbound_mail` and the wasm manager's per-record
    /// ingest — are the production callers; public so an app's tier_1 test
    /// can stand the condition up without a feed. Idempotent; notifies only on
    /// a new entry, so a re-poll of the same page re-renders nothing.
    pub fn note_unopenable_mail(&self, mailbox: MailFeed, uid: u32) {
        let inserted = self
            .unopenable_mail
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((mailbox, uid));
        if inserted {
            self.notify();
        }
    }

    /// The record at `(mailbox, uid)` opened after all — retire its skip
    /// (a no-op, no notify, when it was never skipped). Called by the same
    /// drivers on every opened record, so a re-drain after the account's keys
    /// changed clears exactly the entries it resolved.
    pub fn retire_unopenable_mail(&self, mailbox: MailFeed, uid: u32) {
        let removed = self
            .unopenable_mail
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(mailbox, uid));
        if removed {
            self.notify();
        }
    }

    /// Re-list the community-room invitations standing for this account — the
    /// receive loop's step, beside the inbox drain whose kind they ride
    /// (`ConversationsSession::start_receive_loop`). Quiet by contract: a rail
    /// that cannot list them (no room-ceremony seam — web's declared absence)
    /// or a nest that did not answer leaves the list as it was, and nothing is
    /// surfaced from a background pass. Notifies only when the list moved.
    pub async fn refresh_room_invitations(&self) {
        let Some(backend) = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned() else {
            return;
        };
        let listed = match backend.room_invitations().await {
            Ok(listed) => listed,
            Err(e) => {
                tracing::debug!("room invitations were not listed this sweep: {e:?}");
                return;
            }
        };
        let changed = {
            let mut standing = self.room_invitations.write().unwrap();
            if *standing == listed {
                false
            } else {
                *standing = listed;
                true
            }
        };
        if changed {
            self.notify();
        }
    }

    /// Found a community room from the new-thread composer and send its first
    /// message — [`Self::send_new_thread`]'s arm for a composer whose home-nest
    /// choice makes the room one its nest reads.
    ///
    /// The thread gets a routing key no other thread can hold: the participant
    /// key an MLS thread starts under would hand this room a plain conversation
    /// with the same people (and hand them this room), and the room's channel
    /// is not known until the ceremony answers. It is re-keyed to that channel
    /// before anything routes on it.
    ///
    /// Two failures, told apart by whether the room is bound: a founding that
    /// failed leaves nothing behind — the empty thread is dropped and the
    /// composer keeps the draft, the reason in its send slot — while a room
    /// that was founded and then failed an invitation is real, keeps its
    /// thread, sends the message, and says on the page who was not invited.
    async fn found_room_from_compose(
        &self,
        compose: ComposeState,
        participants: Vec<TypedAddress>,
    ) -> Result<Option<ThreadId>, BackendError> {
        static FOUNDINGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(&Rail::FaunaMls)
            .cloned()
            .ok_or(BackendError::NotSupported)?;
        // The topic the user typed names the room — the one field of the new
        // room's policy this composer offers. Everyone who joins sees it.
        let name = compose
            .subject_draft
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let founding = FOUNDINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = self.threads.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: format!("founding-{founding}"),
            },
            participants.clone(),
            ThreadFlavor::MlsGroup,
            name.clone(),
        );
        // The founder resolved and accepted every one of these: the owner's
        // own gesture, the one handle provenance a succession's tier 2 may
        // dial (`ThreadStore::mark_anchor_grade`).
        self.threads
            .mark_anchor_grade(&id, participants.iter().filter_map(|p| p.person_actor_id()));
        let invite_failure = match backend.found_room(id.clone(), name, &participants).await {
            Ok(()) => None,
            Err(e) if backend.channel_binding_hex(&id).is_none() => {
                self.threads.discard(&id);
                tracing::warn!("founding a community room failed: {e:?}");
                let mut kept = compose;
                kept.send_state = SendState::failed(e.user_detail());
                self.drafts.set_new_thread(Some(kept));
                self.notify();
                return Err(e);
            }
            Err(e) => Some(e),
        };
        if let Some(channel_hex) = backend.channel_binding_hex(&id) {
            self.threads.rekey_to_channel(&id, channel_hex);
        }
        // The body and attachments move to the room's own draft; the topic
        // does not — it became the room's name.
        let has_message = !compose.body_draft.trim().is_empty() || !compose.attachments.is_empty();
        self.drafts.set(
            id.clone(),
            ComposeState {
                body_draft: compose.body_draft.clone(),
                attachments: compose.attachments.clone(),
                ..Default::default()
            },
        );
        self.drafts.set_new_thread(None);
        *self.new_thread_active.write().unwrap() = false;
        *self.selected.write().unwrap() = Some(id.clone());
        self.notify();
        self.persist_thread_history(&id).await;
        let sent = if has_message {
            self.send(id.clone()).await
        } else {
            Ok(())
        };
        // After the send, which clears the page error on entry.
        if let Some(e) = invite_failure {
            tracing::warn!("a community room was founded but an invitation failed: {e:?}");
            self.set_page_error(LocalizedText::key_arg(
                "conversations.unified.error_add_participant",
                "message",
                e.user_detail(),
            ));
        }
        sent.map(|()| Some(id))
    }

    /// The verifying half of [`Self::cache_attachment_bytes`], reporting whether
    /// the pair was accepted so an in-crate receive path can skip the attachment
    /// wholesale rather than render a handle that resolves to nothing.
    ///
    /// Hashes once: callers must NOT pre-check and then call the public door, or
    /// every inbound attachment pays BLAKE3 twice.
    pub(crate) fn cache_attachment_bytes_checked(&self, blob_hash: &str, bytes: Vec<u8>) -> bool {
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != blob_hash {
            // Not an error the user can act on — the sender lied or the bytes were
            // mangled — so it is a log line and a refusal, like a failed decrypt.
            tracing::warn!(
                declared = %blob_hash,
                actual = %actual,
                size_bytes = bytes.len(),
                "attachment bytes do not hash to their declared blob_hash; refusing to cache \
                 (conversations.md § Attachments: the store is content-addressed)"
            );
            return false;
        }
        // The pin set is read by the store itself — at eviction time, under this
        // write lock, after the new entry is resident — never out here before the
        // lock: read here, an insert could take a pin set from before a concurrent
        // `stage_attachment` registered its draft, then evict that draft's
        // just-inserted bytes (`conversations.md` § Attachments → *Retention*).
        // Lock order is store, then drafts; nothing takes the store while holding
        // a drafts lock.
        self.attachments
            .write()
            .unwrap()
            .insert(blob_hash.to_string(), bytes, || {
                self.drafts.staged_attachment_hashes()
            });
        true
    }

    /// Remember where `blob_hash`'s bytes rest, so a render that misses them
    /// after eviction can have them fetched again (`store::attachments`). The
    /// writers are the two receive paths — the FaunaMls receive loop and the
    /// SMTP ingest (`backends::smtp::ingest_inbound_record`) — the send, for
    /// the sender's own FaunaMls attachments ([`Self::send`], off
    /// `SendOutcome::attachment_coordinates`: a sender never walks its own
    /// record back), and a restored `history/<ch>` slice
    /// ([`Self::restore_channel_slice`]).
    pub(crate) fn remember_attachment_coordinates(
        &self,
        blob_hash: String,
        coordinates: AttachmentCoordinates,
    ) {
        self.attachments
            .write()
            .unwrap()
            .remember(blob_hash, coordinates);
    }

    /// The FaunaMls handles renders have missed since the last cycle, each
    /// with its channel and sealed-blob coordinates — what
    /// `refill_evicted_attachments` fetches again. Mail wants stay wanted for
    /// the mail sweep.
    pub(crate) fn take_wanted_sealed_blob_attachments(
        &self,
    ) -> Vec<(String, fauna_mls::types::ChannelId, SealedBlobCoordinates)> {
        self.attachments.write().unwrap().take_wanted_sealed_blobs()
    }

    /// The mail handles renders have missed since the last cycle, each with
    /// the record it rests in — what `refill_evicted_mail_attachments`
    /// re-reads. FaunaMls wants stay wanted for the conversation sweep.
    pub(crate) fn take_wanted_mail_attachments(&self) -> Vec<(String, MailRecordCoordinates)> {
        self.attachments.write().unwrap().take_wanted_mail_records()
    }

    /// A refill found the bytes gone for good: stop remembering them, so the
    /// handle stays declared instead of being asked for every cycle.
    pub(crate) fn forget_attachment_coordinates(&self, blob_hash: &str) {
        self.attachments.write().unwrap().forget(blob_hash);
    }

    /// Install the receive loop's run-one-now poke
    /// ([`crate::session::ConversationsSession::poke_receive_cycle`]) so a
    /// render-time miss on an evicted attachment is refilled promptly. Plain
    /// Rust, not UniFFI — the native session wires it in-process.
    pub fn set_attachment_refill_poke(&self, poke: Arc<dyn Fn() + Send + Sync>) {
        *self.attachment_refill_poke.write().unwrap() = Some(poke);
    }

    /// Test-only: shrink the attachment store's budget so a handful of small
    /// attachments exercise eviction (`ATTACHMENT_STORE_BUDGET_BYTES` is the
    /// production value, a constant). Takes effect at the next insert.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_attachment_store_budget_for_test(&self, budget: usize) {
        self.attachments.write().unwrap().set_budget(budget);
    }

    /// The attachment plaintext the store currently holds, in bytes.
    pub fn attachment_store_resident_bytes(&self) -> usize {
        self.attachments.read().unwrap().resident_bytes()
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl ConversationsManager {
    #[cfg_attr(feature = "uniffi", uniffi::constructor)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            backends: RwLock::new(HashMap::new()),
            threads: ThreadStore::new(),
            drafts: DraftStore::new(),
            contacts: ContactsCache::new(),
            observers: RwLock::new(Vec::new()),
            selected: RwLock::new(None),
            selected_message: RwLock::new(None),
            new_thread_active: RwLock::new(false),
            sort: RwLock::new(SortOrder::default()),
            search: RwLock::new(None),
            revealed_remote: RwLock::new(HashSet::new()),
            policy_anchor_wants: RwLock::new(HashMap::new()),
            parked_floor_deletes: RwLock::new(HashMap::new()),
            reactions: RwLock::new(HashMap::new()),
            deleted: RwLock::new(HashSet::new()),
            delete_claims: RwLock::new(HashMap::new()),
            add_participant: RwLock::new(None),
            attachments: RwLock::new(AttachmentStore::new()),
            attachment_refill_poke: RwLock::new(None),
            mail_read: Mutex::new(MailReadSync::default()),
            mail_read_poke: RwLock::new(None),
            resolved_previews: RwLock::new(HashMap::new()),
            link_preview_rpc: RwLock::new(None),
            local_detections: RwLock::new(None),
            index_observer: RwLock::new(None),
            read_positions: RwLock::new(ReadPositionsSlot::default()),
            peer_anchor_store: RwLock::new(None),
            refused_changes: Arc::default(),
            contact_overlays: RwLock::new(ContactOverlaySlot::default()),
            page_error: RwLock::new(None),
            engine_served_elsewhere: RwLock::new(false),
            receive_loop_generation: std::sync::atomic::AtomicU64::new(0),
            receive_stopped_generation: std::sync::atomic::AtomicU64::new(0),
            unopenable_mail: RwLock::new(std::collections::BTreeSet::new()),
            room_invitations: RwLock::new(Vec::new()),
            identity_epoch: RwLock::new(0),
        })
    }

    /// How many received mail records this process skipped because they would
    /// not open under the account's complete standing key set
    /// ([`Self::note_unopenable_mail`]) — a standing truth each app's
    /// `error-message` projection shows below every other page error
    /// (`ui/conversations.md` § Errors & edge cases): the mailbox keeps
    /// receiving past such a record, and the user is told that some mail did
    /// not open on this device. `0` clears it. Not cleared by any gesture; a
    /// record opening later (a re-drain after the account's keys changed)
    /// retires its entry.
    pub fn unopenable_mail_count(&self) -> u32 {
        self.unopenable_mail
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Whether the receive loop currently serving this manager **died by
    /// panic** — a standing condition each app's `error-message` projection
    /// shows directly under [`Self::engine_served_elsewhere`], telling the user
    /// to restart the app (`ui/conversations.md` § Errors & edge cases). Why a
    /// restart and not a re-armed loop: `ReceiveLoopExit::Panicked`.
    ///
    /// Only the *current* loop counts: a newer loop started over this manager
    /// clears it, and a superseded loop dying late never sets it. A designed
    /// exit (the session dropped, the engine handed over) never sets it at all.
    /// Lock-free, so a page render can never panic on a lock the dead pass
    /// poisoned.
    pub fn receive_stopped(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        let stopped = self.receive_stopped_generation.load(SeqCst);
        stopped != 0 && stopped == self.receive_loop_generation.load(SeqCst)
    }

    /// Record whether this process is a non-holder of the conversations-engine
    /// role (`MlsError::ServedElsewhere` at engine construction). Called by the
    /// app's engine-construction site on every attempt — success clears it,
    /// `ServedElsewhere` sets it, any other engine-init failure leaves it
    /// cleared (a different, unrelated failure). A no-op (no `notify`) when
    /// the value is unchanged, mirroring [`Self::clear_page_error`]'s
    /// no-op-when-unset optimization.
    pub fn set_engine_served_elsewhere(&self, served: bool) {
        {
            let mut slot = self.engine_served_elsewhere.write().unwrap();
            if *slot == served {
                return;
            }
            *slot = served;
        }
        self.notify();
    }

    /// See [`Self::set_engine_served_elsewhere`]. Read by each app's
    /// `error-message` projection with top precedence — a standing condition
    /// that must never be masked by an unrelated gesture's `page_error`.
    pub fn engine_served_elsewhere(&self) -> bool {
        *self.engine_served_elsewhere.read().unwrap()
    }

    pub fn snapshot(&self) -> ConversationsSnapshot {
        let sort = *self.sort.read().unwrap();
        let search_query = self.search.read().unwrap().clone();
        let mut threads = self.threads.list_summaries();
        filter_summaries(&mut threads, search_query.as_deref(), &self.threads);
        sort_summaries(&mut threads, sort);
        self.project_bridge_identities(&mut threads);
        ConversationsSnapshot {
            threads,
            sort,
            search_query,
            selected_thread_id: self.selected.read().unwrap().clone(),
            // Surface the persisted new-thread draft as the shown composer ONLY
            // while the composer is the active detail view; otherwise the draft
            // is kept (recoverable via `+`) but the selected thread shows.
            new_thread_compose: if *self.new_thread_active.read().unwrap() {
                self.drafts.new_thread()
            } else {
                None
            },
            add_participant: self.add_participant.read().unwrap().clone(),
            error: self.page_error.read().unwrap().clone(),
            launch_floor_ms: self.threads.launch_floor_ms(),
            bridges: self.bridge_identities(),
            room_invitations: self
                .room_invitations
                .read()
                .unwrap()
                .iter()
                .map(|invitation| crate::room::RoomInvitationSnapshot {
                    id: invitation.id,
                    inviter_display: self.seat_address_for(invitation.inviter).display(),
                    role: invitation.role,
                })
                .collect(),
        }
    }

    /// The `data.conversation_threads` e2e state rows, JSON-encoded — the
    /// manager-level twin of [`crate::session::ConversationsSession::conversation_threads_json`],
    /// for a caller that has a manager but no (or not yet activated) session:
    /// apple's e2e login path deliberately never activates a real
    /// `ConversationsSession` (`ConversationsVM.activate` — kept unset so the
    /// deterministic `test-helpers` mock backends stay in effect), so an app
    /// reading only off `session?.conversationThreadsJson()` sees an
    /// empty list forever regardless of what `inject_inbound_for_test` staged
    /// (a refactor regression — every apple conversations e2e test
    /// broke the same way, on both macOS and iOS, the moment it switched off
    /// the manager-backed row builder). Delegates to the same
    /// [`crate::state_json::conversation_threads_json`] free function, so the
    /// row shape (including `participant_actor_ids`) is identical either way.
    pub fn conversation_threads_json(&self) -> String {
        crate::state_json::conversation_threads_json(self).to_string()
    }

    /// The `data.conversation_sort` e2e state value, JSON-encoded — the list's
    /// active order in `setSort`'s serde spelling, from the same
    /// [`crate::state_json::conversation_sort_json`] tui and linux call in
    /// process, so no app spells the order names itself.
    pub fn conversation_sort_json(&self) -> String {
        crate::state_json::conversation_sort_json(self).to_string()
    }

    /// Every Fauna identity on any of this member's thread rosters — each once,
    /// in thread-store order.
    ///
    /// The **rosters**, deliberately not [`Self::snapshot`]: that is the
    /// conversations page's view and applies the page's search filter, so a
    /// consumer walking it sees only the threads the search box currently
    /// shows. The peer-anchor harvest sweep is the caller this exists for —
    /// who it harvests must not depend on what the owner last typed into a
    /// filter, least of all since a succession statement can wait on that
    /// harvest (`identity-succession.md` § The succession statement → *the
    /// harvest wait*).
    pub fn fauna_roster_actors(&self) -> Vec<ActorId> {
        let mut seen = std::collections::HashSet::new();
        let mut actors = Vec::new();
        for summary in self.threads.list_summaries() {
            let Some(detail) = self.threads.get(&summary.thread_id) else {
                continue;
            };
            for participant in &detail.participants {
                if let TypedAddress::Fauna { actor_id, .. } = participant
                    && seen.insert(*actor_id)
                {
                    actors.push(*actor_id);
                }
            }
        }
        actors
    }

    pub fn thread_detail(&self, id: ThreadId) -> Option<ThreadDetail> {
        self.threads.get(&id).map(|mut d| {
            d.compose = self.drafts.get(&id);
            // The viewer's own nickname for a fauna-native sender paints as the
            // bubble's sender (contacts.md § The private overlay → *Where the
            // nickname paints*). Read-time, so a nickname set or cleared
            // re-paints on the next emit; a message with no nickname keeps
            // `sender_display` empty and every app's existing fallback to the
            // address display. FIRST, because the reply-quote fold below names
            // a quoted parent by its `sender_display`.
            self.project_sender_nicknames(&mut d.messages);
            // And on the member chips, index-parallel with `participants`.
            self.project_participant_nicknames(&mut d);
            // D2b: project an in-bubble reply-quote onto each reply
            // (render-model.md § D2 QuotedMessage). A read-time projection like
            // the D3 reveal below — the parent may be ingested *after* the reply,
            // so resolving here (against the thread's live message list) lets a
            // late parent fill the quote on the next emit, and hides it cleanly
            // when the parent isn't loaded (user-approved 2026-06-23).
            fold_reply_quotes(&mut d.messages);
            // D4 (render-model.md § D4): project the resolved link-preview state onto each
            // bubble's `LinkPreview` block FIRST — the producer emits the block `Resolving`;
            // once `resolve_link_preview` records a terminal state for its URL, every
            // `thread_detail` flips the matching bubble block to `Resolved`/`Failed`, so the
            // client walks one authoritative document, never an out-of-band preview map. This
            // MUST run before the D3 reveal walk below: the cached `PreviewState::Resolved`
            // carries `revealed:false`, so folding it in after the reveal walk would clobber a
            // just-revealed og:image (the feed snapshot uses the same ordering). A no-op walk
            // when nothing is resolved (the common case).
            let previews = self.resolved_previews.read().unwrap();
            if !previews.is_empty() {
                for m in &mut d.messages {
                    for block in &mut m.document.blocks {
                        if let fauna_core::render::RenderBlock::LinkPreview { url, state } = block
                            && let Some(resolved) = previews.get(url)
                        {
                            *state = resolved.clone();
                        }
                    }
                }
            }
            // D3 + D4 reveal: project the manager-owned per-message reveal set onto each
            // message's document (render-model.md § D3). The set — not a per-app
            // dictionary — decides whether a message's `RemoteImage`s AND its Resolved
            // link-preview og:image render fetched; `reveal_remote_images` flips it and
            // re-emits. A no-op walk when nothing is revealed (the common case).
            let revealed = self.revealed_remote.read().unwrap();
            if !revealed.is_empty() {
                for m in &mut d.messages {
                    if revealed.contains(&m.message_id) {
                        m.document.set_remote_images_revealed(true);
                    }
                }
            }
            // Project manager-owned reaction log and delete tombstones onto each
            // message snapshot (conversations.md § Reactions & message delete).
            self.project_reactions_and_deletes(&mut d.messages);
            // The selected message (`search.md` § State & data shape — the second
            // half of `SearchNav::Mail`), RESOLVED against this thread's fetched
            // window rather than passed through. Three things fall out of doing
            // it here instead of at selection time, and each is load-bearing:
            //
            // 1. A selection naming a message this thread does not hold — the
            //    stale id left behind when `send_new_thread` / the 1:1
            //    participant fork selects a *different* thread — projects to
            //    `None`, so no app can paint a marker it cannot place. This
            //    read is the single guard for that; the write sites deliberately
            //    do not each re-clear, so the invariant lives in one place.
            // 2. A hit on a message that has not arrived yet (the thread's
            //    history streams in) lights up by itself on the emit that
            //    fetches it, with no retry plumbing in any of the 7 apps.
            // 3. Every app gets the same answer from one projection, so
            //    "which message is selected" is never re-derived per app
            //    (priority #2) — the app's whole job is to paint the flag.
            d.selected_message_id = self
                .selected_message
                .read()
                .unwrap()
                .clone()
                .filter(|id| d.messages.iter().any(|m| &m.message_id == id));
            self.project_room(&mut d);
            // The delete affordance, per message (`MessageSnapshot::can_delete`):
            // after the room, because who governs is the room's answer.
            let deletes_any = d.room.as_ref().is_some_and(|r| r.viewer_deletes_any());
            // Assigned on every message, never only raised: a slice persisted
            // from a projection may carry an answer a role change has outdated.
            let supported = d.capabilities.supports_message_delete;
            for m in &mut d.messages {
                m.can_delete = supported && (m.is_own || deletes_any);
            }
            d
        })
    }

    /// The hex nest-channel id a FaunaMls thread is bound to (once its MLS group
    /// has bootstrapped / a Welcome materialized it), or `None` for an unbound
    /// thread. Surfaced into the e2e state protocol so a real-wire test can
    /// observe the channel an MLS conversation carries on the nest.
    pub fn channel_hex(&self, id: &ThreadId) -> Option<String> {
        self.threads.channel_hex(id)
    }
}

/// Outside the `uniffi::export` block: `RoomPostRoom` is the room-post seam's
/// own type (`fauna_core::room_post`), read by the feed in shared Rust through
/// `ConversationsSession`'s `RoomPostKeys` impl — never across FFI. Likewise
/// the harvest offer's writer, whose `ChannelId` key never crosses it either.
impl ConversationsManager {
    /// The room (`conversation-rooms.md` § The room): asked of the thread's
    /// rail on every emit, and the one place the roles table becomes gating
    /// (`RoomSnapshot::gate`) — so no app ever computes a class or a role
    /// (§ Architectural rules 1).
    ///
    /// **Two callers, one function**, like the fold below:
    /// [`Self::thread_detail`] and [`Self::thread_state_facts`] must answer
    /// the same room, and the second hands the rail a detail with no messages
    /// — which is sound because a rail's `room_state` reads the thread's
    /// identity and participants, never its messages (the trait says so).
    ///
    /// **The rail's capability answer comes first** (`RailBackend::
    /// capabilities`): the bridged rail overlays the vector its bridge
    /// declared there (`conversations.md` § Where logic lives → *The `Bridged`
    /// adapter*, ruling 2 (b)), and the room's gate narrows what it answers.
    /// The bridge identity rides the same projection (ruling 2 (a)).
    fn project_room(&self, d: &mut ThreadDetail) {
        let Some(backend) = self.backends.read().unwrap().get(&d.rail).cloned() else {
            return;
        };
        d.capabilities = backend.capabilities(d);
        if let Some(room) = backend.room_state(d) {
            d.capabilities = room.gate(d.capabilities);
            d.room = Some(room);
        }
        d.bridge = crate::backends::bridged::bridge_id_of(&d.participants)
            .and_then(|id| backend.bridge_identity(id));
        if let Some(bridge) = &d.bridge {
            d.glyph = bridge.glyph;
        }
        d.guardian_state = backend.guardian_state(&d.participants);
    }

    /// The bridges serving this account, by declared identity and ordered by
    /// label — for the recipient picker, which names them so the user knows
    /// which far networks a typed address may reach, and labels a resolved
    /// bridged address with its bridge. Empty with no bridged rail registered
    /// or no bridge consented.
    pub fn bridge_identities(&self) -> Vec<crate::snapshot::BridgeIdentitySnapshot> {
        self.backends
            .read()
            .unwrap()
            .get(&Rail::Bridged)
            .map(|backend| backend.bridge_identities())
            .unwrap_or_default()
    }

    /// The bridge identity of every [`Rail::Bridged`] summary, from the rail's
    /// registry — [`Self::project_room`]'s identity half for the list, where
    /// the glyph a bridged row paints is the one its bridge declared.
    fn project_bridge_identities(&self, threads: &mut [crate::snapshot::ThreadSummary]) {
        let Some(backend) = self.backends.read().unwrap().get(&Rail::Bridged).cloned() else {
            return;
        };
        for summary in threads.iter_mut().filter(|t| t.rail == Rail::Bridged) {
            summary.bridge = self
                .threads
                .bridge_id_of(&summary.thread_id)
                .and_then(|id| backend.bridge_identity(&id));
            if let Some(bridge) = &summary.bridge {
                summary.glyph = bridge.glyph;
            }
            summary.guardian_state = self
                .threads
                .participants_of(&summary.thread_id)
                .and_then(|participants| backend.guardian_state(&participants));
        }
    }

    /// What a state reader needs of thread `id` — its identity, participants
    /// and their displays, its room and the room-gated capabilities, exactly as
    /// [`Self::thread_detail`] projects them — plus its message count and
    /// subject lines, WITHOUT cloning a single message.
    ///
    /// `thread_detail` clones every message's body, rendered document and
    /// attachments, then walks them for the render-time projections (reply
    /// quotes, previews, reveals, reactions). The e2e state serializer
    /// ([`crate::state_json::conversation_threads_json`]) reads none of that,
    /// and every app's state provider calls it on every publish — on linux
    /// twenty times a second, on the GTK main thread. Measured 2026-09-21:
    /// with one ~3 MiB inbound mail in the mailbox, each publish cost ~230 ms,
    /// the thread never went idle, and every barrier ack and dialog map
    /// beneath DEFAULT priority starved (`apps/linux.md` § Message Flow).
    pub fn thread_state_facts(&self, id: &ThreadId) -> Option<ThreadStateFacts> {
        let (mut detail, message_count, subject_lines) = self.threads.get_without_messages(id)?;
        self.project_room(&mut detail);
        Some(ThreadStateFacts {
            detail,
            message_count,
            subject_lines,
        })
    }

    /// Fold the manager-owned reaction log and delete tombstones onto
    /// `messages` (`conversations.md` § Reactions & message delete): the
    /// aggregate replaces `reactions`, and `deleted` is set — never cleared —
    /// by this device's own delete set or by an inbound claim the fold admits
    /// ([`DeleteClaim::admits`] — the sender match, or a governing role the
    /// rail recorded when the delete was made).
    ///
    /// **Two callers, deliberately one function.** [`Self::thread_detail`]
    /// projects it for the apps to render; [`Self::snapshot_channel_slice`]
    /// projects the *same* fold into the `history/<ch>` replica, because this
    /// state is manager memory that no stream replay can rebuild on a restored
    /// device (`devices.md` § Durability rules, rule 3). A second copy of the
    /// fold would be a second answer to "is this message deleted" — the app's
    /// and the replica's — and they would drift.
    ///
    /// A no-op walk when all three collections are empty (the common case).
    fn project_reactions_and_deletes(&self, messages: &mut [MessageSnapshot]) {
        let me = self.me_actor();
        let logs = self.reactions.read().unwrap();
        let deleted = self.deleted.read().unwrap();
        let claims = self.delete_claims.read().unwrap();
        if logs.is_empty() && deleted.is_empty() && claims.is_empty() {
            return;
        }
        for m in messages {
            if let (Some(me), Some(log)) = (me, logs.get(&m.message_id)) {
                m.reactions = fold_reactions(log, me);
            }
            if deleted.contains(&m.message_id)
                || claims
                    .get(&m.message_id)
                    .is_some_and(|cs| cs.iter().any(|c| c.admits(sender_actor(m))))
            {
                m.deleted = true;
            }
        }
    }

    /// Replace the policy names `channel` offers the peer-anchor harvest
    /// ([`Self::policy_anchor_wants`]). The room backend is the one writer: it
    /// filters to names an **anchored** policy version carries and bounds the
    /// list, so nothing the room's home nest merely served reaches the sweep.
    pub fn set_policy_anchor_wants(
        &self,
        channel: fauna_mls::types::ChannelId,
        names: Vec<ActorId>,
    ) {
        let mut wants = self.policy_anchor_wants.write().unwrap();
        if names.is_empty() {
            wants.remove(&channel);
        } else {
            wants.insert(channel, names);
        }
    }

    /// Record one more floor delete record the room backend has parked on
    /// `channel` — the at-rest mirror of its live park
    /// ([`Self::parked_floor_deletes`]): a union, so a pass that re-judges a
    /// restored set and re-parks part of it never narrows what the next slice
    /// carries below what this device still holds.
    pub fn park_floor_delete(
        &self,
        channel: fauna_mls::types::ChannelId,
        record: crate::store::history::ParkedFloorDelete,
    ) {
        let mut parked = self.parked_floor_deletes.write().unwrap();
        crate::store::history::union_parked_floor_deletes(
            parked.entry(channel).or_default(),
            [record],
        );
    }

    /// Replace `channel`'s parked floor delete records at rest with the room
    /// backend's live set — what a pass leaves parked once every record it
    /// took up has been judged ([`Self::parked_floor_deletes`]).
    pub fn set_parked_floor_deletes(
        &self,
        channel: fauna_mls::types::ChannelId,
        records: Vec<crate::store::history::ParkedFloorDelete>,
    ) {
        let mut parked = self.parked_floor_deletes.write().unwrap();
        if records.is_empty() {
            parked.remove(&channel);
        } else {
            parked.insert(channel, records);
        }
    }

    /// `channel`'s parked floor delete records at rest — restored from the
    /// replica or mirrored from the live set — for the room backend to take up
    /// on its next pass ([`Self::parked_floor_deletes`]).
    pub fn parked_floor_deletes(
        &self,
        channel: &fauna_mls::types::ChannelId,
    ) -> Vec<crate::store::history::ParkedFloorDelete> {
        self.parked_floor_deletes
            .read()
            .unwrap()
            .get(channel)
            .cloned()
            .unwrap_or_default()
    }

    /// Every community-room policy name this device wants a succession anchor
    /// for — each once, rooms in id order so the walk is stable across passes
    /// (`identity-succession.md` § The succession statement → *a community
    /// policy's names join the harvest's walk*).
    pub fn policy_anchor_wants(&self) -> Vec<ActorId> {
        let wants = self.policy_anchor_wants.read().unwrap();
        let mut rooms: Vec<_> = wants.iter().collect();
        rooms.sort_unstable_by_key(|(channel, _)| channel.0);
        let mut seen = std::collections::HashSet::new();
        rooms
            .into_iter()
            .flat_map(|(_, names)| names)
            .filter(|name| seen.insert(**name))
            .copied()
            .collect()
    }

    /// Who the peer-anchor harvest sweep walks: [`Self::fauna_roster_actors`],
    /// then [`Self::policy_anchor_wants`], then what each registered rail
    /// asks for (`RailBackend::harvest_anchor_wants` — the FaunaMls rail's
    /// folder-channel owners) — each identity once. The policy names are
    /// *pushed* by the room backend because only a walk discovers them; the
    /// rail wants are *pulled* because they derive from durable engine state
    /// and are right from the first pass of a launch.
    pub fn harvest_walk_actors(&self) -> Vec<ActorId> {
        let mut actors = self.fauna_roster_actors();
        let rail_wants: Vec<ActorId> = self
            .backends
            .read()
            .unwrap()
            .values()
            .flat_map(|backend| backend.harvest_anchor_wants())
            .collect();
        for name in self.policy_anchor_wants().into_iter().chain(rail_wants) {
            if !actors.contains(&name) {
                actors.push(name);
            }
        }
        actors
    }

    /// The rooms a new **room-restricted post** can be addressed to from this
    /// device (`ui/feed.md` § Encryption at rest → *Room-restricted — the
    /// ruling*; `RoomPostKeys::room_post_rooms`): every group thread bound to
    /// a channel whose room is of a member-keyed class — end-to-end or
    /// community — labelled as the conversation list reads it, sorted by
    /// label.
    ///
    /// The class is the rail's own `room_state` answer, the one place a class
    /// is ever decided (§ Architectural rules 1 of `conversation-rooms.md`),
    /// never re-derived here. A 1:1 is not a group (`conversation-rooms.md`
    /// § The room) and a mail thread has no channel, so neither is offered; a
    /// transport-only room is not member-keyed, so it has no key to seal for.
    ///
    /// A room this device **no longer holds a seat on** — as far as the rail
    /// can tell (`RailBackend::seated_on_room`: an end-to-end room whose group
    /// this device has processed its own removal from) — is not offered
    /// either: the seal would be refused at send, so offering it is a dead
    /// option. This is composer honesty, not a gate — a device that never saw
    /// its removal has nothing to answer from, the residue `ui/feed.md`
    /// ruling 5 states (*the key is the gate* cuts both ways).
    pub fn room_post_rooms(&self) -> Vec<fauna_core::room_post::RoomPostRoom> {
        let mut rooms: Vec<_> = self
            .threads
            .bound_threads()
            .into_iter()
            .filter(|(_, _, flavor)| *flavor == crate::thread::ThreadFlavor::MlsGroup)
            .filter_map(|(id, channel_hex, _)| {
                let room = hex::decode(channel_hex)
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok())?;
                // Asked outside the store's lock, as `thread_detail` asks it.
                let detail = self.threads.get(&id)?;
                let backend = self.backends.read().unwrap().get(&detail.rail).cloned()?;
                let class = backend.room_state(&detail)?.class;
                (matches!(
                    class,
                    crate::room::RoomClass::EndToEnd | crate::room::RoomClass::Community
                ) && backend.seated_on_room(&detail))
                .then_some(fauna_core::room_post::RoomPostRoom {
                    room,
                    label: detail.label,
                })
            })
            .collect();
        rooms.sort_by(|a, b| a.label.cmp(&b.label).then(a.room.cmp(&b.room)));
        rooms
    }
}

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl ConversationsManager {
    pub fn add_observer(&self, obs: Arc<dyn SnapshotObserver>) {
        self.observers.write().unwrap().push(obs);
    }

    /// The home-screen widget's number (`apps/common.md` § Home-screen
    /// widget): [`crate::snapshot::sum_unread`] over **every** thread of the
    /// account — the unfiltered store, not [`Self::snapshot`]'s list, which a
    /// typed search narrows. A widget outside the app reports the account's
    /// unread, and a transient filter inside the app must not move it. One
    /// getter for every app's outside-the-app surface, so none keeps a tally of
    /// its own or runs a second count query. Reads the thread store under its
    /// own lock, so — like every snapshot read — an observer calls it after
    /// the notifying mutation unwinds, never inline on the mutator's thread.
    pub fn unread_total(&self) -> u32 {
        crate::snapshot::sum_unread(&self.threads.list_summaries())
    }

    /// The Status page's `status-mls-channels` count —
    /// [`crate::snapshot::secure_channel_count`] over the live thread store,
    /// under the same lock discipline as [`Self::unread_total`] (a read, never
    /// inline on a mutator's thread). The shared snapshot's MLS leg
    /// (`ui/status.md` § State & data shape) takes this number as its input.
    pub fn secure_channel_count(&self) -> u64 {
        crate::snapshot::secure_channel_count(&self.threads.list_summaries())
    }

    /// Drop all registered observers. Each GTK client attaches a fresh observer
    /// per authenticated-window build (`views/conversations` → `observer::attach`),
    /// and nothing prunes the old ones — so across a sign-out→re-auth cycle they
    /// accumulate unboundedly, and (because the linux authenticated window's
    /// widget tree does not finalize on `destroy()`) the *stale* panes their
    /// receiver loops hold stay live in the a11y tree, re-presenting dialogs and
    /// fielding clicks against a now-shut-down runtime. Call this at sign-out
    /// (before the window is rebuilt): dropping the observers closes each stale
    /// loop's channel, so its `rx.recv()` returns `Err` and the loop breaks,
    /// releasing its panes. The next window build re-attaches a live observer.
    pub fn clear_observers(&self) {
        self.observers.write().unwrap().clear();
    }

    /// Number of registered snapshot observers. Diagnostic-only — surfaced through
    /// windows' e2e state protocol (`diagnostics.conversations_observer_count`) so a
    /// per-manager observer-accumulation regression (a client constructing a fresh
    /// observer on every re-navigation instead of reusing one for the manager's
    /// lifetime) fails as itself in a headless assertion instead of as a downstream
    /// dead-dispatch/retention symptom found later.
    // (kept out of the `///` doc comment above — this is
    // a UniFFI-exported item, and the transform's excision would otherwise change
    // its doc-comment checksum)
    pub fn observer_count(&self) -> u32 {
        self.observers.read().unwrap().len() as u32
    }

    /// The identity epoch: advanced by every [`Self::clear_for_identity_change`].
    /// A shell reads it **before** starting a detached read of account-scoped
    /// state (the launch `__drafts` fetch) and hands it back with the result
    /// ([`Self::restore_drafts_at`]), which refuses a result the outgoing
    /// account started. Opaque: only equality with a later read means anything.
    pub fn identity_epoch(&self) -> u64 {
        *self
            .identity_epoch
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Wipe every piece of identity-scoped state this manager holds, preserving
    /// registered backends and observers. Call it on the **production** account
    /// switch / sign-out path, before the incoming identity's session activates:
    /// the manager is an identity-scoped singleton that outlives the shell
    /// teardown, so a switch that skipped this would render the outgoing
    /// account's threads, drafts and selection to the incoming one.
    ///
    /// Deliberately NOT `_for_test`-gated. It is the production twin of the
    /// harness's [`Self::clear_for_test`] (which delegates here), and it exists
    /// because the two callers are genuinely different: the e2e harness wipes
    /// *between tests*, apple's `tearDownSessionForSwitch` and linux's
    /// identity-switch path wipe *between identities*. Apple reached the seam
    /// for the production job while every apple FFI recipe still shipped
    /// `test-helpers`, which `testing.md` § convention 15's recipe split removes
    /// — so the production path needs a production method, not a wider gate.
    /// Named to match the identity-change helpers linux already has
    /// (`critical_alerts::clear_for_identity_change`, `screen_lock::…`).
    ///
    /// It also advances [`Self::identity_epoch`], which retires every launch
    /// restore the outgoing account started ([`Self::restore_drafts_at`]).
    pub fn clear_for_identity_change(&self) {
        let mut epoch = self
            .identity_epoch
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *epoch = epoch.wrapping_add(1);
        // The read-position seam is the outgoing account's store: retire it
        // before the wipe, so no delivery of its positions lands after it.
        // The incoming account registers its own at its store-ready edge.
        {
            let mut slot = self.read_positions.write().unwrap();
            slot.generation += 1;
            slot.seam = None;
        }
        // So are the peer anchors: the incoming account's witness must never
        // read — or write — the outgoing account's heads.
        *self.peer_anchor_store.write().unwrap() = None;
        // The overlay seam, its verdicts and its projection are the outgoing
        // account's too.
        {
            let mut slot = self.contact_overlays.write().unwrap();
            slot.generation += 1;
            slot.folds = None;
            slot.successions.clear();
        }
        // The refused-change log and anything held for it are the outgoing
        // account's notices.
        self.refused_changes.clear();
        self.contacts.replace(Default::default());
        self.threads.clear();
        self.drafts.clear_all();
        *self.selected.write().unwrap() = None;
        *self.selected_message.write().unwrap() = None;
        *self.search.write().unwrap() = None;
        *self.sort.write().unwrap() = SortOrder::default();
        *self.add_participant.write().unwrap() = None;
        // A page error left standing would make the next identity (or the next
        // test) start with an `error-message` it never caused.
        *self.page_error.write().unwrap() = None;
        self.reactions.write().unwrap().clear();
        self.deleted.write().unwrap().clear();
        self.delete_claims.write().unwrap().clear();
        // Owed `\Seen` writes and the flag cursor name the outgoing account's
        // mailbox.
        *self.mail_read.lock().unwrap() = MailReadSync::default();
        drop(epoch);
        self.notify();
    }

    // ── Selection ───────────────────────────────────────────────

    pub fn select_thread(&self, id: ThreadId) {
        // Viewing an existing thread deactivates the new-thread composer VIEW but
        // PRESERVES its draft (conversations.md § Persistence — switching keeps
        // each half-written message; only cancel/send discards). Per-thread
        // drafts likewise persist (the DraftStore keeps them keyed by thread id),
        // so each conversation keeps its own half-written reply across switches.
        *self.new_thread_active.write().unwrap() = false;
        *self.selected.write().unwrap() = Some(id);
        // A plain thread selection carries no message selection. Clearing here
        // (rather than only on the way out) is what keeps the marker from
        // surviving onto a thread the user picked by hand.
        *self.selected_message.write().unwrap() = None;
        self.notify();
    }

    /// Select a thread **and** one message inside it — the whole of
    /// `SearchNav::Mail`'s contract (`docs/goal/ui/search.md` § State & data
    /// shape), and the only producer of a message selection.
    ///
    /// One write pair + one [`Self::notify`], so observers never see the
    /// intermediate state where the thread has flipped but the message has not:
    /// on an app that scrolls to the selection, that intermediate frame is a
    /// visible jump to the wrong place.
    ///
    /// The `message_id` is **not** validated here. The thread's messages arrive
    /// asynchronously, so a hit on a not-yet-fetched message would fail a
    /// constructor-time check and lose a selection that becomes valid moments
    /// later; [`Self::thread_detail`] resolves it against the live window on
    /// every emit instead, which also makes a late arrival light up by itself.
    pub fn select_thread_and_message(&self, thread_id: ThreadId, message_id: MessageId) {
        *self.new_thread_active.write().unwrap() = false;
        *self.selected.write().unwrap() = Some(thread_id);
        *self.selected_message.write().unwrap() = Some(message_id);
        self.notify();
    }

    pub fn clear_selection(&self) {
        *self.selected.write().unwrap() = None;
        *self.selected_message.write().unwrap() = None;
        self.notify();
    }

    // ── Sort & search ────────────────────────────────────────────
    //
    // `conversation-sort` / `conversation-search-box` (conversations.md §
    // User actions). `set_sort` reorders the thread list and `set_search_query`
    // filters it — both applied in `snapshot()` (via `sort_summaries` /
    // `filter_summaries`), so every app renders the already-sorted,
    // already-filtered `threads` (priority #3). This realizes the thread-list
    // *filtering* the original messaging-UI design deferred as a follow-on
    // (tracked internally: "global search across threads is not in this
    // slice").

    pub fn set_sort(&self, order: SortOrder) {
        *self.sort.write().unwrap() = order;
        self.notify();
    }

    pub fn set_search_query(&self, query: Option<String>) {
        *self.search.write().unwrap() = query;
        self.notify();
    }

    // ── Remote-image reveal (D3) ─────────────────────────────────
    //
    // `load-remote-content-button` (html-mail.md § Element IDs). The reveal
    // state lives here — one manager-owned set — not in a per-app
    // `revealedRemote`/`remoteLoaded` dictionary (render-model.md § D3): the
    // privacy-sensitive "when does untrusted inbound content phone home"
    // decision is made in one audited place. `thread_detail` projects the set
    // onto `RemoteImage.revealed`.

    /// Opt this message into loading its remote images. Adds it to the in-memory
    /// reveal set and re-emits, so the next [`thread_detail`](Self::thread_detail)
    /// projects `RemoteImage.revealed: true` for it. **In-memory only** — the
    /// no-persistence posture (html-mail.md § Rendering) is unchanged; a reveal
    /// does not survive a restart. Idempotent: a repeat tap is a no-op insert
    /// plus a harmless re-emit.
    pub fn reveal_remote_images(&self, message_id: MessageId) {
        self.revealed_remote.write().unwrap().insert(message_id);
        self.notify();
    }

    // ── Draft persistence (v2) ───────────────────────────────────

    /// The whole conversations-rail draft set serialised to its canonical
    /// at-rest bytes. The client glue seals these under the owner's `BackupKey`
    /// and uploads them to the `__drafts` reserved folder after a compose
    /// change (`docs/goal/behavior/file-sync.md` § Drafts Sync). Byte-stable for
    /// equal logical state, so an unchanged draft set re-uploads identically.
    pub fn drafts_snapshot_bytes(&self) -> Vec<u8> {
        self.drafts.snapshot_bytes()
    }

    // ── Compose mutators (per-thread drafts) ─────────────────────

    pub fn set_compose_body(&self, id: ThreadId, body: String) {
        let mut s = self.drafts.get(&id);
        s.body_draft = body;
        self.drafts.set(id, s);
        self.notify();
    }

    pub fn toggle_topic(&self, id: ThreadId) {
        let mut s = self.drafts.get(&id);
        s.subject_draft = match s.subject_draft {
            None => Some(String::new()),
            Some(_) => None,
        };
        self.drafts.set(id, s);
        self.notify();
    }

    pub fn set_compose_subject(&self, id: ThreadId, subject: String) {
        let mut s = self.drafts.get(&id);
        s.subject_draft = Some(subject);
        self.drafts.set(id, s);
        self.notify();
    }

    pub fn set_reply_to(&self, id: ThreadId, msg: Option<MessageId>) {
        let mut s = self.drafts.get(&id);
        let clearing = msg.is_none();
        s.reply_to = msg;
        // Cancelling the reply (`dm-reply-cancel`) also clears the editable
        // To line — a plain (non-reply) send falls back to the thread
        // participants, so an empty `reply_recipients` is the right idle state.
        if clearing {
            s.reply_recipients.clear();
        }
        self.drafts.set(id, s);
        self.notify();
    }

    /// What thread `id`'s compose bar previews for the reply in progress
    /// (`dm-reply-preview`): the answered message's sender and a plain-text
    /// excerpt, resolved against the thread's fetched messages. `None` when no
    /// reply is armed — and when the answered message is not in the fetched
    /// window, since an empty preview beats a stale or wrong one. One
    /// derivation for every app: until 2026-09-21 tui and apple painted the
    /// body, windows the sender's name, and web, android and linux the bare
    /// message id.
    pub fn reply_preview(&self, id: ThreadId) -> Option<crate::compose::ReplyPreview> {
        let detail = self.thread_detail(id)?;
        let answered = detail.compose.reply_to.as_ref()?;
        let message = detail.messages.iter().find(|m| &m.message_id == answered)?;
        // Named as the bubble's `dm-sender` names it: `sender_display` until it is
        // empty (no contact-name resolution yet), then the shared
        // `TypedAddress::display` every app's bubble already falls back to.
        let sender_display = if message.sender_display.is_empty() {
            message.sender.display()
        } else {
            message.sender_display.clone()
        };
        Some(crate::compose::ReplyPreview {
            sender_display,
            excerpt: crate::store::threads::snippet_preview(&message.body),
        })
    }

    /// Seed a reply draft on `id` to `msg_id` (`dm-reply-button` /
    /// `dm-reply-all-button`). Always sets `compose.reply_to`. On rails with
    /// `supports_recipient_selection` (mail) it also seeds the editable To line
    /// (`compose.reply_recipients`): `reply_all == false` → the replied
    /// message's sender only; `reply_all == true` → every thread participant
    /// except the local user (`backend.self_address()`). On other rails the To
    /// line is hidden, so `reply_recipients` stays empty (recipients ARE the
    /// thread membership). Either seed is then editable via
    /// [`Self::add_reply_recipient`] / [`Self::remove_reply_recipient`]
    /// (`conversations.md` § Participants vs reply recipients).
    pub fn start_reply(&self, id: ThreadId, msg_id: MessageId, reply_all: bool) {
        let Some(detail) = self.thread_detail(id.clone()) else {
            return;
        };
        let mut s = self.drafts.get(&id);
        s.reply_to = Some(msg_id.clone());
        if detail.capabilities.supports_recipient_selection {
            let self_addr = self
                .backends
                .read()
                .unwrap()
                .get(&detail.rail)
                .and_then(|b| b.self_address());
            s.reply_recipients = if reply_all {
                // Reply-all = every historical participant but self.
                detail
                    .participants
                    .iter()
                    .filter(|p| self_addr.as_ref().is_none_or(|me| !me.same_address(p)))
                    .cloned()
                    .collect()
            } else {
                // Reply = the sender of the replied-to message only.
                detail
                    .messages
                    .iter()
                    .find(|m| m.message_id == msg_id)
                    .map(|m| vec![m.sender.clone()])
                    .unwrap_or_default()
            };
        }
        self.drafts.set(id, s);
        self.notify();
    }

    /// Add `addr` to the editable reply To line (`dm-reply-recipient-add`),
    /// de-duplicated by address identity. No-op if already present.
    pub fn add_reply_recipient(&self, id: ThreadId, addr: TypedAddress) {
        let mut s = self.drafts.get(&id);
        if !s.reply_recipients.iter().any(|r| r.same_address(&addr)) {
            s.reply_recipients.push(addr);
            self.drafts.set(id, s);
            self.notify();
        }
    }

    /// Remove `addr` from the editable reply To line
    /// (`dm-reply-recipient-remove`). Drops the recipient from *this reply
    /// only* — thread participants and history are untouched.
    pub fn remove_reply_recipient(&self, id: ThreadId, addr: TypedAddress) {
        let mut s = self.drafts.get(&id);
        let before = s.reply_recipients.len();
        s.reply_recipients.retain(|r| !r.same_address(&addr));
        if s.reply_recipients.len() != before {
            self.drafts.set(id, s);
            self.notify();
        }
    }

    // ── Attachments ──────────────────────────────────────────────
    //
    // `attachment-button` → the client opens its native file picker and hands
    // the picked file's bytes here (`docs/goal/ui/conversations.md` § User
    // actions). The bytes are cached in the attachment store under their BLAKE3
    // `blob_hash`; only the light metadata draft lands on `ComposeState`, so the
    // observed snapshot stays cheap. `send` re-resolves the bytes from the store.

    /// Stage `bytes` as an attachment on `id`'s compose draft. Returns the
    /// `blob_hash` the bytes were cached under (the handle the client renders /
    /// the wire message references). Capability gating (`supports_attachments`)
    /// is the client's responsibility — a rail that drops attachments just won't
    /// inline them on `send`.
    pub fn add_attachment(
        &self,
        id: ThreadId,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> String {
        let blob_hash = self.stage_attachment(filename, mime_type, bytes, |draft| {
            let mut s = self.drafts.get(&id);
            s.attachments.push(draft);
            self.drafts.set(id, s);
        });
        self.notify();
        blob_hash
    }

    /// Remove the staged attachment at `index` from `id`'s compose draft (the
    /// `dm-attachment-*` remove affordance on the compose bar). The cached bytes
    /// are left in the store — harmless (in-memory, cleared on next login) and
    /// keeps the remove cheap; a shared blob could still be referenced elsewhere.
    pub fn remove_attachment(&self, id: ThreadId, index: u32) {
        let mut s = self.drafts.get(&id);
        let i = index as usize;
        if i < s.attachments.len() {
            s.attachments.remove(i);
            self.drafts.set(id, s);
            self.notify();
        }
    }

    /// The shared attachment loader: the plaintext bytes cached under
    /// `blob_hash`, if any (populated by `add_attachment`, the send echo, and the
    /// inbound parse). The per-app render path resolves a
    /// `dm-attachment-image[i]` / `-file[i]` to real bytes through this. `None`
    /// for an unknown, not-yet-fetched, or **evicted** hash — the store is a
    /// bounded cache (`store::attachments`), and every app already paints the
    /// declared filename + size when the bytes are absent. A miss on a handle
    /// whose coordinates are remembered (a FaunaMls blob this device received or sent, a mail
    /// record, or either restored from a `history/<ch>` slice) marks it wanted
    /// and pokes the receive loop, whose next cycle fetches it again on that
    /// rail's sweep and notifies observers, so the same render asks again and
    /// hits.
    pub fn attachment_bytes(&self, blob_hash: String) -> Option<Vec<u8>> {
        let read = self.attachments.write().unwrap().read(&blob_hash);
        match read {
            AttachmentRead::Hit(bytes) => Some(bytes),
            AttachmentRead::Miss { wanted_now } => {
                let poke = self.attachment_refill_poke.read().unwrap().clone();
                if wanted_now && let Some(poke) = poke {
                    poke();
                }
                None
            }
        }
    }

    /// Whether `blob_hash`'s bytes are resident in this device's attachment store
    /// right now — a read-only peek that neither copies the bytes nor marks a miss
    /// wanted. An app that reuses a bubble across renders keys its rebuild on this
    /// as well as on the message: evicting bytes or fetching them again changes no
    /// message, so a bubble keyed on the message alone keeps painting what it
    /// painted before (`conversations.md` § Attachments → *Retention*).
    pub fn attachment_resident(&self, blob_hash: String) -> bool {
        self.attachments.read().unwrap().contains(&blob_hash)
    }

    /// Cache an inbound attachment's plaintext bytes under its `blob_hash`. The
    /// receive path ([`crate::backends::smtp::ingest_inbound_record`], and the
    /// wasm JS-driven poll's equivalent) calls this for each extracted attachment
    /// before ingesting the message, so the rendered bubble resolves the handle
    /// to real bytes via [`Self::attachment_bytes`].
    ///
    /// **The key is verified, not trusted.** `blob_hash` must be the lowercase-hex
    /// BLAKE3 of `bytes` — the invariant `conversations.md` § Attachments states —
    /// and a pair that does not satisfy it is **refused with a warn**, exactly as
    /// an undecryptable attachment is skipped. Silent rather than fallible on
    /// purpose: this method is `uniffi::export`ed, so a `Result` would change the
    /// FFI signature for all 7 apps to report a condition none of them can act on.
    ///
    /// Verifying here rather than at each caller is what makes the guarantee
    /// hold: this is the ONLY door into the store, it is a public FFI surface, and
    /// the store is a single map shared across every channel and room. Before
    /// this, `blob_hash` on the FaunaMls path was **sender-authored** (it rides
    /// inside the sealed `ChannelAttachment`) and unchecked, so any co-member who
    /// knew a hash could overwrite those bytes for every conversation at once —
    /// and, because `send` re-resolves bytes from this same map by hash
    /// (`resolve_attachments`), could swap a victim's *staged outgoing* file
    /// between staging and send, leaving them to seal and sign it under their own
    /// filename. Both arms close here: substituting bytes under a fixed key now
    /// requires a BLAKE3 preimage.
    ///
    /// Idempotent for a truthful pair — re-caching bytes under their own hash is
    /// a harmless overwrite (it is now genuinely the same value).
    pub fn cache_attachment_bytes(&self, blob_hash: String, bytes: Vec<u8>) {
        let _ = self.cache_attachment_bytes_checked(&blob_hash, bytes);
    }

    // ── New-thread compose ──────────────────────────────────────

    pub fn start_new_conversation(&self) {
        // Activate the new-thread composer. PRESERVE any in-progress new-thread
        // draft (recipients/subject/body) so re-opening `+` after switching away
        // restores it — conversations.md § Persistence: a half-written new
        // message survives switching; only an explicit cancel or a successful
        // send clears it. Only seed a fresh empty composer when none is stashed.
        if self.drafts.new_thread().is_none() {
            self.drafts.set_new_thread(Some(ComposeState {
                recipient_picker: Some(RecipientPickerState::default()),
                ..Default::default()
            }));
        }
        *self.new_thread_active.write().unwrap() = true;
        self.notify();
    }

    pub fn cancel_new_conversation(&self) {
        // The explicit Cancel/discard affordance — the one path, alongside a
        // successful send, that drops the new-thread draft (conversations.md
        // § Persistence). Deactivate the composer view and clear the draft.
        self.drafts.set_new_thread(None);
        *self.new_thread_active.write().unwrap() = false;
        self.notify();
    }

    pub fn deactivate_new_conversation(&self) {
        // Nav-back out of the new-thread composer without selecting a thread
        // (a mobile back button / a pane dismiss). Deactivate the composer VIEW
        // but PRESERVE the draft so re-opening `+` (`start_new_conversation`)
        // restores it — conversations.md § Persistence: a plain back keeps the
        // half-written message; only an explicit `cancel_new_conversation` or a
        // successful send discards it. This is the no-thread-selected counterpart
        // of `select_thread`'s deactivate (which preserves the draft the same way
        // but also selects the clicked thread).
        *self.new_thread_active.write().unwrap() = false;
        self.notify();
    }

    /// Update the new-thread picker's raw input. **Typing owes a probe**: any
    /// non-empty input parks the picker on `Resolving` until the async
    /// [`Self::resolve_recipient`] reports; empty input is `Idle`. The state is
    /// never derived from the text's *shape* — a shape-derived `Resolved` said
    /// "Resolved" for a Fauna peer whose lookup had not started, and let Enter
    /// commit an email chip for it (`docs/goal/ui/conversations.md` § Errors &
    /// edge cases → *The picker tells the truth*, 2026-08-29).
    pub fn set_new_thread_recipient_input(&self, text: String) {
        if let Some(mut s) = self.drafts.new_thread() {
            let mut picker = s.recipient_picker.unwrap_or_default();
            picker.resolve_state = unprobed_resolve_state(&text);
            picker.raw_input = text;
            // New input invalidates any prior async resolution.
            picker.resolved = None;
            s.recipient_picker = Some(picker);
            self.drafts.set_new_thread(Some(s));
            self.notify();
        }
    }

    pub fn accept_new_thread_chip(&self, addr: TypedAddress) {
        if let Some(mut s) = self.drafts.new_thread() {
            let mut picker = s.recipient_picker.unwrap_or_default();
            picker.chips.push(addr);
            picker.raw_input = String::new();
            // The picker now holds at least one resolved chip. Keep the
            // resolve-state reflecting that: the most recent attempt
            // succeeded. A subsequent `set_new_thread_recipient_input`
            // call (typing the next address) re-derives state from the
            // new raw input and may move it back to Idle / Error.
            picker.resolve_state = ResolveState::Resolved;
            // The chip consumed the resolution; clear it so the next typed
            // address doesn't inherit a stale resolved value.
            picker.resolved = None;
            s.recipient_picker = Some(picker);
            self.drafts.set_new_thread(Some(s));
            self.notify();
        }
    }

    pub fn set_new_thread_body(&self, body: String) {
        if let Some(mut s) = self.drafts.new_thread() {
            s.body_draft = body;
            self.drafts.set_new_thread(Some(s));
            self.notify();
        }
    }

    pub fn set_new_thread_subject(&self, subject: Option<String>) {
        if let Some(mut s) = self.drafts.new_thread() {
            s.subject_draft = subject;
            self.drafts.set_new_thread(Some(s));
            self.notify();
        }
    }

    /// `recipient-picker-home-nest-toggle` — whether the room about to be
    /// created seats the user's home nest, which makes it a **community** room
    /// the first send founds (`conversation-rooms.md` § The three classes).
    /// The picker's class statement follows it
    /// ([`crate::room::prospective_room_class`]). A no-op with no new-thread
    /// compose open.
    pub fn set_new_thread_home_nest(&self, include: bool) {
        if let Some(mut s) = self.drafts.new_thread() {
            let mut picker = s.recipient_picker.unwrap_or_default();
            picker.include_home_nest = include;
            s.recipient_picker = Some(picker);
            self.drafts.set_new_thread(Some(s));
            self.notify();
        }
    }

    /// Stage an attachment on the new-thread compose (`attachment-button` before
    /// a thread exists). Mirrors [`Self::add_attachment`] for the single-slot
    /// new-thread draft; `send_new_thread` carries the staged attachments onto
    /// the materialized thread's draft. `None` if no new-thread compose is open.
    pub fn add_new_thread_attachment(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Option<String> {
        let mut s = self.drafts.new_thread()?;
        let blob_hash = self.stage_attachment(filename, mime_type, bytes, |draft| {
            s.attachments.push(draft);
            self.drafts.set_new_thread(Some(s));
        });
        self.notify();
        Some(blob_hash)
    }

    /// Remove the staged attachment at `index` from the new-thread compose.
    pub fn remove_new_thread_attachment(&self, index: u32) {
        if let Some(mut s) = self.drafts.new_thread() {
            let i = index as usize;
            if i < s.attachments.len() {
                s.attachments.remove(i);
                self.drafts.set_new_thread(Some(s));
                self.notify();
            }
        }
    }

    // ── Add-participant overlay ──────────────────────────────────

    /// Open the add-participant overlay for `id`. No-op if the thread
    /// doesn't exist. The picker starts empty (`Idle`).
    ///
    /// The thread lookup that answers "does it exist" also answers "does
    /// confirming reach the wire"
    /// ([`crate::capabilities::is_in_place_mls_group`]), so the overlay
    /// carries its own offline-gate discriminant from the moment it opens and
    /// no client has to re-derive it
    /// ([`AddParticipantState::in_place_mls_group`]).
    pub fn open_add_participant(&self, id: ThreadId) {
        let Some(detail) = self.threads.get(&id) else {
            return;
        };
        *self.add_participant.write().unwrap() = Some(AddParticipantState {
            target_thread_id: id,
            picker: RecipientPickerState::default(),
            in_place_mls_group: crate::capabilities::is_in_place_mls_group(
                detail.rail,
                detail.flavor.clone(),
            ),
        });
        self.notify();
    }

    /// Update the add-participant picker's raw input. No-op if the overlay
    /// isn't open. Same *typing owes a probe* rule as
    /// [`Self::set_new_thread_recipient_input`].
    pub fn set_add_participant_recipient_input(&self, text: String) {
        let mut guard = self.add_participant.write().unwrap();
        let Some(state) = guard.as_mut() else { return };
        state.picker.resolve_state = unprobed_resolve_state(&text);
        state.picker.raw_input = text;
        // New input invalidates any prior async resolution.
        state.picker.resolved = None;
        drop(guard);
        self.notify();
    }

    /// Commit `addr` as a chip on the add-participant picker (clears the
    /// raw input). No-op if the overlay isn't open. Mirrors
    /// `accept_new_thread_chip`.
    pub fn accept_add_participant_chip(&self, addr: TypedAddress) {
        let mut guard = self.add_participant.write().unwrap();
        let Some(state) = guard.as_mut() else { return };
        state.picker.chips.push(addr);
        state.picker.raw_input = String::new();
        state.picker.resolve_state = ResolveState::Resolved;
        state.picker.resolved = None;
        drop(guard);
        self.notify();
    }

    /// Close the add-participant overlay, discarding any in-progress input.
    pub fn cancel_add_participant(&self) {
        *self.add_participant.write().unwrap() = None;
        self.notify();
    }

    /// Accept the current text on whichever recipient picker is active as a
    /// chip — the add-participant overlay's picker takes priority over the
    /// new-thread picker. Returns `true` iff a chip was pushed. Single entry
    /// point for the "press Enter" / "click suggestion" path on every app (and
    /// the e2e test command).
    ///
    /// **Commits only a probed address.** The chip is the picker's `resolved`
    /// address — what the async [`Self::resolve_recipient`] confirmed, carrying
    /// the real rail/identity — and nothing else: no shape parse of the raw
    /// text from `Resolving` / `Error` / `NotFound` / `Idle`. Enter before the
    /// probe lands, or after it errored, commits nothing (the status line says
    /// why); an app whose Enter path wants to be robust resolves first and then
    /// accepts (web, linux, tui's agent path). The former shape fallback was one
    /// of the four points at which a Fauna peer that did not answer became a
    /// silently-accepted email chip (`docs/goal/ui/conversations.md` § Errors &
    /// edge cases → *The picker tells the truth*, 2026-08-29).
    pub fn accept_current_recipient_chip(&self) -> bool {
        // Snapshot the picker field and release the read lock *before* calling
        // the accept_*_chip mutators, which need the write lock.
        let ap: Option<Option<TypedAddress>> = self
            .add_participant
            .read()
            .unwrap()
            .as_ref()
            .map(|s| s.picker.resolved.clone());
        if let Some(resolved) = ap {
            if let Some(addr) = resolved {
                self.accept_add_participant_chip(addr);
                return true;
            }
            // add-participant overlay is open but nothing is resolved —
            // return early so the new-thread picker doesn't steal the action.
            return false;
        }
        if let Some(compose) = self.drafts.new_thread()
            && let Some(picker) = compose.recipient_picker
            && let Some(addr) = picker.resolved
        {
            self.accept_new_thread_chip(addr);
            return true;
        }
        false
    }

    // ── Membership / rename ──────────────────────────────────────

    /// Add a participant to a thread.
    ///
    /// For `(FaunaMls, OneToOne)` this forks a new MLS group thread (the
    /// original 1:1 stays intact) — an MLS-protocol necessity: a pairwise
    /// MLS group can't gain a member, so the only path to a 3-person
    /// encrypted thread is a fresh group. Every other `(rail, flavor)` adds
    /// the participant in place: for SMTP this is just a wider CC, and for a
    /// bridge whose vector declares it, the far network's own add; there is
    /// no cryptographic fork. (A bridge declaring none has
    /// `supports_membership_change == false`, so this is unreachable for
    /// it.) Returns the (possibly new) thread id.
    pub fn add_participant(&self, id: ThreadId, addr: TypedAddress) -> Option<ThreadId> {
        let r = self.add_participant_inner(id, addr);
        self.notify();
        r
    }

    fn add_participant_inner(&self, id: ThreadId, addr: TypedAddress) -> Option<ThreadId> {
        let detail = self.threads.get(&id)?;
        // The person the owner just resolved and added — the owner's own
        // gesture, so their handle is anchor-grade for a succession's tier 2
        // (`ThreadStore::mark_anchor_grade`). The rows carried over from the
        // 1:1 thread keep whatever provenance they had there.
        let added = addr.person_actor_id();
        match (detail.rail, detail.flavor) {
            (Rail::FaunaMls, ThreadFlavor::OneToOne) => {
                let mut new_participants = detail.participants.clone();
                new_participants.push(addr);
                let key = ThreadKey::Participants {
                    rail: Rail::FaunaMls,
                    participants: new_participants.clone(),
                };
                let group = self.threads.find_or_create(
                    key,
                    new_participants,
                    ThreadFlavor::MlsGroup,
                    None,
                );
                self.threads.mark_anchor_grade(&group, added);
                self.threads.inherit_anchor_grade(&group);
                Some(group)
            }
            // Non-FaunaMls, or already a group / subject-keyed thread: add
            // in place. The flavor is left unchanged — a 3-party
            // participant-keyed SMTP thread keeping the OneToOne tag is a
            // cosmetic misnomer that doesn't affect capabilities (SMTP caps
            // don't depend on flavor); a OneToOne→SubjectKeyed transition is
            // a follow-up.
            _ => {
                self.threads.add_participant_to(&id, addr);
                self.threads.mark_anchor_grade(&id, added);
                Some(id)
            }
        }
    }

    /// Read thread `id` without opening it. Opening one needs no call: the
    /// selection itself reads it ([`Self::notify`]), so an app that also calls
    /// this on select is redundant, never wrong.
    pub fn mark_read(&self, id: ThreadId) {
        self.read_thread(&id);
        self.notify();
    }

    // ── Inbound ingestion ────────────────────────────────────────

    /// Ingest one decrypted inbound message: bucket it via the rail backend,
    /// resolve its thread (by participants / subject / reply-reference), and
    /// append it.
    ///
    /// **Message-ID dedup.** A message whose id is already held in the store is
    /// skipped (no second append). This is what keeps a server-side **Sent** copy
    /// from double-showing in-session: a first-party `fauna.email.send` is echoed
    /// locally on send (`Self::send`, keyed by the RFC `Message-ID` the client
    /// minted), and the SAME message then arrives back over `fauna.email.sent.fetch`
    /// as nest's durable Sent copy (`smtp-server.md` § Inbound client receive) — it
    /// carries that identical `Message-ID`, so the dedup suppresses the duplicate
    /// while leaving the message in the view. The caller's per-feed `seen` set
    /// (`poll_inbound_mail`) keys on the server *segment-record* id and so can't
    /// catch this (the local echo has no segment id); this id-level guard is the
    /// only thing that can. After a client restart the in-memory store is empty, so
    /// the Sent copy ingests normally and the sent message reloads — the durability
    /// the server-side copy exists for. Idempotent across overlapping re-polls too.
    pub fn ingest_inbound(&self, msg: RailInboundMessage) -> Result<(), BackendError> {
        self.ingest_inbound_identified(msg, None, None)
    }
}

/// The identity-carrying ingest — outside the `uniffi::export` block above
/// because its `Option<&[u8]>` is not an FFI type and never needs to be: the
/// only caller holding a nest segment-record id is the shared Rust receive
/// path (`backends::smtp::ingest_inbound_record`).
impl ConversationsManager {
    /// [`Self::ingest_inbound`] plus the nest's segment-record id for this
    /// message, when the calling frame holds one. It rides only to the
    /// content-index sink as the doc's *secondary* identity
    /// ([`crate::index_sink::IndexableMessage::nest_message_id`]); threading,
    /// dedup and the snapshot are untouched by it. A parameter rather than a
    /// `RailInboundMessage` field on purpose: that struct is a UniFFI record,
    /// and this id never needs to cross the FFI boundary inbound.
    ///
    /// `mail` is the same kind of non-FFI passthrough for what the mail record
    /// says beyond the message (`backends::smtp::ingest_inbound_record`): its
    /// server-side [`MailFeed`] — [`RailBackend::bucket_inbound`]'s provenance
    /// signal — plus its UID and `\Seen` flag, which decide whether the
    /// message is unread and name it for the flag's write
    /// (`conversation-read-state.md` § Mail: `\Seen` is the marker).
    pub fn ingest_inbound_identified(
        &self,
        msg: RailInboundMessage,
        nest_message_id: Option<&[u8]>,
        mail: Option<crate::store::threads::MailArrival>,
    ) -> Result<(), BackendError> {
        let mailbox = mail.map(|m| m.mailbox);
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(&msg.rail)
            .cloned()
            .ok_or(BackendError::NotSupported)?;
        // What the record parsed to beyond the snapshot `bucket_inbound` builds,
        // taken before `msg` moves into it: the Sent-copy upgrade below compares
        // it, and the append at the end keeps it beside the held copy
        // (`ThreadStore::append_inbound_message`).
        let facts = crate::store::threads::InboundParseFacts {
            recipients: msg.recipients.clone(),
            subject: msg.subject.clone(),
            body_format: msg.body_format,
        };
        let bucket = backend.bucket_inbound(msg, mailbox)?;
        // Dedup by message id before any thread create/append (see the doc note
        // above) — we already hold this exact message (local echo / earlier poll).
        if self
            .threads
            .thread_for_message(&bucket.message.message_id)
            .is_some()
        {
            // An `INBOX` record never displaces a held copy: it can claim no
            // more than the copy already held, so the duplicate is dropped —
            // whether it is a re-poll, a self-send's delivery copy arriving
            // after the Sent copy, or a squat arriving after the genuine
            // message (`conversations.md:1572`).
            if !bucket.message.is_own {
                return Ok(());
            }
            // Self-addressed mail lands in both `INBOX` and `Sent` under the
            // same Message-ID; `INBOX` is polled first (`session.rs:1416-1431`)
            // and normally reaches here first too, so the guard above usually
            // fires on the later `Sent` copy. That copy's own `is_own` — this
            // account's mailbox provenance, never a forgeable header — is proof
            // of authorship the earlier `INBOX` copy couldn't claim for itself.
            // The Message-ID collision rule (`conversations.md:1572`): the
            // `Sent` copy is the message — content, ownership and delivery
            // time. A held copy that parses to the same message upgrades in
            // place (`ThreadStore::mark_message_own`: `is_own` flips, the
            // Sent copy's timestamp is adopted); one that differs in any
            // compared field is a squat on the account's Message-ID and is
            // displaced — evicted from its thread (the thread too, when that
            // empties it and it binds nothing else), after which the Sent copy
            // falls through to the ordinary ingest below and threads by its
            // own key. So the forged content never renders as own, and the
            // account's genuine message is never hidden behind it.
            match self.threads.mark_message_own(&bucket.message, &facts) {
                SentCopyOutcome::Upgraded => {
                    self.notify();
                    return Ok(());
                }
                // The held copy is already the account's own — this device's
                // send echo, or this same Sent copy ingested earlier. The echo
                // was never offered to the content index (`Self::send` leaves
                // mail to this copy: a mail doc's secondary identity is the nest
                // record id, which only the Sent copy carries), so offer it now,
                // in the session it was sent in. A re-poll of an already-offered
                // copy presents the same `(kind, content_id, record id)` and the
                // builder's stage-time guard drops it.
                SentCopyOutcome::AlreadyOwn => {
                    if let Some(thread_id) =
                        self.threads.thread_for_message(&bucket.message.message_id)
                    {
                        self.observe_for_index(
                            index_kind_for_rail(bucket.rail),
                            &thread_id,
                            &bucket.message,
                            bucket.subject.as_deref(),
                            nest_message_id,
                        );
                    }
                    return Ok(());
                }
                SentCopyOutcome::NotComparable | SentCopyOutcome::NotHeld => return Ok(()),
                SentCopyOutcome::Squat => {
                    if let Some(evicted) = self.threads.evict_message(&bucket.message.message_id)
                        && evicted.emptied
                        && backend.channel_binding_hex(&evicted.thread_id).is_none()
                    {
                        self.threads.discard(&evicted.thread_id);
                    }
                    tracing::warn!(
                        "a held INBOX copy squatted on the Message-ID of a message this \
                         account sent; the Sent copy displaces it"
                    );
                }
            }
        }
        let key = key_for_inbound(
            bucket.rail,
            bucket.participants.clone(),
            bucket.subject.as_deref(),
            bucket.in_reply_to.as_ref(),
        );
        let thread_id = match &key {
            ThreadKey::ByMessageReference(parent) => {
                self.threads.thread_for_message(parent).unwrap_or_else(|| {
                    // The referenced parent isn't in any thread yet — an unknown
                    // / mismatched Message-ID (e.g. the original was sent from a
                    // different MUA whose Message-ID we never stored, or it
                    // simply hasn't been ingested yet). Don't strand the reply in
                    // a bare participants thread (which left the original
                    // "missing" from the reply's thread in manual testing); fall
                    // back to the SAME key a non-reply would get — subject when
                    // present — so the reply still merges with its conversation by
                    // normalized subject (standard References→subject threading).
                    let (fallback_key, label) = match bucket
                        .subject
                        .as_deref()
                        .map(normalize_subject)
                        .filter(|s| !s.is_empty())
                    {
                        Some(subject) => (
                            ThreadKey::SubjectKeyed {
                                rail: bucket.rail,
                                participants: bucket.participants.clone(),
                                subject,
                            },
                            bucket.subject.clone(),
                        ),
                        None => (
                            ThreadKey::Participants {
                                rail: bucket.rail,
                                participants: bucket.participants.clone(),
                            },
                            None,
                        ),
                    };
                    self.threads.find_or_create(
                        fallback_key,
                        bucket.participants.clone(),
                        infer_flavor(&bucket),
                        label,
                    )
                })
            }
            _ => self.threads.find_or_create(
                key.clone(),
                bucket.participants.clone(),
                infer_flavor(&bucket),
                // Preserve the original-case subject as the display label
                // for subject-keyed threads (the key still uses the
                // normalized lowercase form for matching).
                bucket
                    .subject
                    .clone()
                    .filter(|_| matches!(key, ThreadKey::SubjectKeyed { .. })),
            ),
        };
        // Subject-divider derivation: compare to last message's effective subject
        let mut msg = bucket.message;
        if let Some(detail) = self.threads.get(&thread_id) {
            let last_subject = detail
                .messages
                .iter()
                .rev()
                .find_map(|m| m.subject_line.clone())
                .or_else(|| {
                    if let ThreadKey::SubjectKeyed { subject, .. } = &key {
                        Some(subject.clone())
                    } else {
                        None
                    }
                });
            if let Some(s) = bucket.subject.as_ref() {
                let normalized = crate::keying::normalize_subject(s);
                if !normalized.is_empty() && Some(&normalized) != last_subject.as_ref() {
                    msg.subject_line = Some(s.clone());
                }
            }
        }
        // Content-index sink (`crate::index_sink`): fired here rather than at
        // the mail rail's `backends::smtp::ingest_inbound_record` because this
        // frame is past the message-id dedup above (so a duplicate the manager
        // just dropped is never indexed) and `thread_id` exists (so the emitted
        // doc carries a real navigation target). `bucket.subject` is the
        // message's own subject — `msg.subject_line` is set only when the
        // subject *changes*, which would leave most mail with no Title field.
        self.observe_for_index(
            index_kind_for_rail(bucket.rail),
            &thread_id,
            &msg,
            bucket.subject.as_deref(),
            nest_message_id,
        );
        self.threads
            .append_inbound_message(&thread_id, msg, facts, mail);
        self.notify();
        Ok(())
    }
}

// The one method of the internal surface below that DOES cross the FFI
// boundary, in its own exported block because the rest of that surface takes
// Rust-only types (`Arc<dyn RailBackend>`) UniFFI cannot describe.
#[cfg_attr(feature = "uniffi", uniffi::export)]
impl ConversationsManager {
    /// Retire the currently-registered conversations-engine rail
    /// ([`Rail::FaunaMls`]) so a successor can take its place: drop the
    /// registration and call [`RailBackend::retire`] on the way out, which
    /// releases the MLS engine's conversations-engine role lock over
    /// `mls_state.db`.
    ///
    /// **Why this is a separate method and not part of
    /// [`Self::clear_for_identity_change`].** That one deliberately *preserves*
    /// registered backends — its doc says so, and it is right to: it wipes the
    /// identity-scoped *content* a switch must not carry over, while the rails
    /// themselves are re-registered a moment later by the incoming session. This
    /// method does the opposite and rarer thing, and the two run at different
    /// moments: the wipe happens at the identity change, the retire happens
    /// immediately before the successor engine is constructed. Folding them
    /// together would leave a manager with no rails through every switch, and
    /// would still miss the case this exists for — a **same-identity** re-login,
    /// where nothing is identity-changing but a second engine over one
    /// `mls_state.db` is refused all the same.
    ///
    /// Called by the shared native session factory
    /// (`fauna_ffi::FfiNestClient::conversations_session*`) before
    /// `MlsEngine::new`, so every UniFFI app — macOS, iOS, windows, android —
    /// inherits the hand-over with no glue of its own. Idempotent, and a no-op
    /// when no MLS rail is registered (a bare or mock-backed manager).
    ///
    /// The window between this call and the successor's `register_backend` is
    /// deliberate and bounded: it is one engine construction wide, and during it
    /// a send on the MLS rail resolves no backend and fails honestly, which is
    /// the correct answer while the account's engine is mid-hand-over.
    ///
    /// **Also exported over UniFFI, for the shell that DROPS its manager**
    /// (2026-09-02). "Every UniFFI app inherits the hand-over with no glue of
    /// its own" was true only of shells that keep one manager across the
    /// hand-over, because the factory can only retire the manager it is *handed*.
    /// Windows replaces its process-wide manager at an actor change
    /// (`ConversationsManagerHost.ResetForActorChange` — its sanctioned
    /// exception to the no-swap rule, since the outgoing identity's rails,
    /// observers and threads must not survive a switch), and it does so
    /// **before** the successor build — so the factory's retire ran against a
    /// brand-new manager with no MLS rail, took the documented no-op arm, and
    /// the predecessor engine was left to a reference drop. That is the
    /// mechanism this ruling refuses. A shell that drops its manager therefore
    /// calls this on the OUTGOING one first; the call is the same explicit
    /// ordered hand-over, moved to the one seam the factory cannot see.
    pub fn retire_conversations_engine(&self) {
        let previous = self.backends.write().unwrap().remove(&Rail::FaunaMls);
        if let Some(b) = previous {
            b.retire();
        }
    }
}

// Internal Rust-only surface (not crossing the FFI boundary). Backend
// registration is wired up by the platform glue that owns the manager;
// foreign code sees only the public methods above.
impl ConversationsManager {
    pub fn register_backend(&self, b: Arc<dyn RailBackend>) {
        self.backends.write().unwrap().insert(b.rail(), b);
    }

    /// The **refused-change inbox** both inbound rails' sinks record into
    /// ([`crate::refused_changes`]) — handed to each sink at construction; the
    /// host registers its log at the account-store-ready edge.
    pub fn refused_changes(&self) -> Arc<crate::refused_changes::RefusedChangeInbox> {
        Arc::clone(&self.refused_changes)
    }

    /// Register the **private contact overlay** seam
    /// ([`crate::backend::ContactOverlayFolds`], `contacts.md` § The private
    /// overlay) — wired from the app's account-store-ready edge
    /// (`fauna-client-account-runtime`'s one implementation); `None` for a
    /// projection with no store behind it to fold into. Replaces any seam
    /// registered before, keeping this session's verdicts.
    ///
    /// Returns the registration's generation, which every delivery carries
    /// ([`Self::apply_contact_overlays`]).
    pub fn register_contact_overlays(
        &self,
        folds: Option<Arc<dyn crate::backend::ContactOverlayFolds>>,
    ) -> u64 {
        let mut slot = self.contact_overlays.write().unwrap();
        slot.generation += 1;
        slot.folds = folds;
        slot.generation
    }

    /// Register the **peer-anchor store**
    /// ([`crate::backend::PeerAnchorStore`]) — wired from the app's
    /// account-store-ready edge (`fauna-account-seams`' one implementation,
    /// through `conversation_seams::wire`). Replaces any store registered
    /// before; an identity change clears it ([`Self::clear_for_identity_change`]).
    pub fn register_peer_anchor_store(
        &self,
        store: Option<Arc<dyn crate::backend::PeerAnchorStore>>,
    ) {
        *self.peer_anchor_store.write().unwrap() = store;
    }

    /// The registered peer-anchor store, if the account store is ready — the
    /// one route the witness, the harvest and the organizer-succession dialer
    /// read and write the anchors through. `None` reads as an unreadable
    /// store, never an empty one.
    pub fn peer_anchor_store(&self) -> Option<Arc<dyn crate::backend::PeerAnchorStore>> {
        self.peer_anchor_store.read().unwrap().clone()
    }

    /// The current overlay registration's generation — for a writer that
    /// reloads the projection after its own write (the private section's
    /// Save), taken BEFORE the write so a reload racing an identity change is
    /// refused.
    pub fn contact_overlays_generation(&self) -> u64 {
        self.contact_overlays.read().unwrap().generation
    }

    /// Load the private contact overlay projection — the store's
    /// `contact_overlays()` read, re-fed on its change nudges. Re-emits when
    /// anything changed, so every snapshot re-paints the names; then runs the
    /// succession fold's reconcile (`contacts.md` § The private overlay →
    /// *When a person's identity succeeds*: it re-runs whenever the projection
    /// loads a non-empty item under an identity with a verified successor).
    ///
    /// `false` when `generation` is not the current registration's — the
    /// delivery is refused, as [`Self::apply_read_positions`] refuses one.
    pub fn apply_contact_overlays(
        &self,
        generation: u64,
        overlays: std::collections::BTreeMap<String, fauna_core::contact_overlay::ContactOverlay>,
    ) -> bool {
        if self.contact_overlays.read().unwrap().generation != generation {
            return false;
        }
        if self.contacts.replace(overlays) {
            self.notify();
        }
        self.reconcile_overlay_folds();
        true
    }

    /// Ask the fold seam to fold every non-empty overlay whose person has a
    /// verified successor onto that person's terminal successor. A verdict is
    /// consumed, never made, here: only [`Self::apply_inbound_succession`]
    /// records one.
    fn reconcile_overlay_folds(&self) {
        let slot = self.contact_overlays.read().unwrap();
        let Some(folds) = slot.folds.clone() else {
            return;
        };
        if slot.successions.is_empty() {
            return;
        }
        let terminal = |start: ActorId| {
            let mut at = start;
            for _ in 0..MAX_FOLD_HOPS {
                match slot.successions.get(&at) {
                    Some(next) if *next != start => at = *next,
                    _ => break,
                }
            }
            (at != start).then_some(at)
        };
        let asks: Vec<(String, String)> = self
            .contacts
            .people()
            .into_iter()
            .filter_map(|key| {
                let actor = ActorId::from_hex(&key).ok()?;
                Some((key, terminal(actor)?.to_hex()))
            })
            .collect();
        drop(slot);
        for (predecessor, successor) in asks {
            folds.fold(&predecessor, &successor);
        }
    }

    /// The overlay projection, for surfaces outside the conversations
    /// snapshot that name people (the roster, the Profile).
    pub fn contacts(&self) -> Arc<ContactsCache> {
        Arc::clone(&self.contacts)
    }

    /// The bubbles' sender names: the viewer's nickname for a fauna-native
    /// sender. **Proven by the rail, not gated here** (`contacts.md` § The
    /// private overlay → *The paint gate*): a native-rail message's `sender` is
    /// the leaf MLS authenticated at decrypt (`conversations.md` MLS-1 —
    /// `fauna_mls::engine::decrypt` overwrites the self-asserted sender with
    /// it) or a room message's signature-verified author, so the actor id a
    /// bubble carries was bound to its key before it reached this store. The
    /// thread's *current* roster is deliberately not consulted: a departed
    /// member's bubbles and a community room's authors were authenticated when
    /// written, and un-naming them would buy nothing.
    fn project_sender_nicknames(&self, messages: &mut [crate::message::MessageSnapshot]) {
        for m in messages {
            if !m.sender_display.is_empty() {
                continue;
            }
            if let Some(actor) = m.sender.person_actor_id()
                && let Some(nick) = self.contacts.nickname(&actor.to_hex())
            {
                m.sender_display = nick;
            }
        }
    }

    /// The identities the FaunaMls rail has **proven** for `thread` — the
    /// verified leaves of its bound MLS group
    /// ([`crate::backend::RailBackend::authoritative_roster`], every leaf
    /// admitted under `conversations.md` MLS-2's `credential == leaf signature
    /// key`). `None` when the rail keeps no roster for the thread: a thread not
    /// yet bootstrapped (its participants are a local intention, their actor
    /// ids taken from a resolve answer nothing has checked), or a non-MLS
    /// rail. The paint gate (`contacts.md` § The private overlay → *The paint
    /// gate*) and the handle backfill key on this and never on
    /// `ThreadDetail::participants`, for the reason
    /// [`Self::evict_person_everywhere`] carries: the snapshot is a local view
    /// no inbound Commit reconciles.
    fn proven_roster(&self, thread: &ThreadId) -> Option<Vec<ActorId>> {
        // Cloned out of the lock before the call: the backend may take its own
        // locks, and a std guard held across it would serialize every reader.
        let rail = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
        rail.and_then(|b| b.authoritative_roster(thread))
    }

    /// The member chips' names through the one resolver
    /// ([`fauna_core::format::peer_display_label`]): the viewer's nickname for
    /// a Fauna participant, else the address display the store already holds.
    /// Read-time and on the snapshot only — `participants` keeps the public
    /// handle, so nothing authored from the thread ever carries a nickname
    /// (`contacts.md` § The private overlay, guard 2).
    ///
    /// **The paint gate** (`contacts.md` § The private overlay → *The paint
    /// gate*): the nickname paints only on a
    /// participant whose actor id is a verified leaf of the thread's bound
    /// group ([`Self::proven_roster`]). A cross-nest participant's actor id
    /// comes from the dialed nest's `by_handle` answer, which the dial rule
    /// leaves unbound (`federation.md` § Peer-auth model → *What a peer's
    /// answer may and may not claim*) — a nest serving `attacker.test` may
    /// answer `bob` with the id of the person the viewer calls "Mum", and the
    /// KeyPackage the group then fetches from it may be for a key of its own.
    /// So before the group exists, and whenever the admitted leaf is not that
    /// key, the chip keeps the public label the viewer typed.
    fn project_participant_nicknames(&self, detail: &mut ThreadDetail) {
        let proven = self.proven_roster(&detail.thread_id);
        for (participant, display) in detail
            .participants
            .iter()
            .zip(detail.participant_displays.iter_mut())
        {
            let Some(actor) = participant.person_actor_id() else {
                continue;
            };
            if !proven
                .as_ref()
                .is_some_and(|roster| roster.contains(&actor))
            {
                continue;
            }
            let hex = actor.to_hex();
            let nickname = self.contacts.nickname(&hex);
            *display = fauna_core::format::peer_display_label(
                nickname.as_deref(),
                Some(display.as_str()),
                participant.person_handle(),
                &hex,
            )
            .primary;
        }
    }

    /// Register the **read-position** seam
    /// ([`crate::backend::ReadPositions`]) — the outward half of synced read
    /// state on the fauna-native rail (`conversation-read-state.md` § The
    /// read-marker record → *How the manager reaches the plane*). Wired from
    /// the app's account-store-ready edge (`fauna-client-account-runtime`'s
    /// one implementation), never at login: the store resolves after it.
    /// Replaces any seam registered before.
    ///
    /// Returns the registration's generation, which the implementation hands
    /// back with every delivery ([`Self::apply_read_positions`]).
    pub fn set_read_positions(&self, seam: Arc<dyn crate::backend::ReadPositions>) -> u64 {
        let mut slot = self.read_positions.write().unwrap();
        slot.generation += 1;
        slot.seam = Some(seam);
        slot.generation
    }

    /// The inward half: the account store's read positions for the native
    /// rail, `(channel_hex, through)` — the whole kind, an absent channel
    /// meaning nothing read there (`conversation-read-state.md` § How the
    /// carriers meet the in-memory set). From the first delivery on, a native
    /// thread's unread set is its channel position's, not the launch floor's
    /// ([`ThreadStore::apply_read_positions`]); observers hear of it only
    /// when a count actually moved.
    ///
    /// `generation` is what [`Self::set_read_positions`] returned. `false`
    /// means that registration is retired — replaced, or ended by an identity
    /// change — and nothing was applied: the caller stops delivering.
    pub fn apply_read_positions(&self, generation: u64, positions: Vec<(String, u64)>) -> bool {
        // Held across the apply, so an identity change (which retires the
        // slot before it wipes the threads) is either wholly before this
        // delivery or wholly after it.
        let slot = self.read_positions.read().unwrap();
        if slot.generation != generation || slot.seam.is_none() {
            return false;
        }
        let changed = self.threads.apply_read_positions(positions);
        drop(slot);
        if changed {
            self.notify();
        }
        true
    }

    /// Restore a `history/<ch>` replica slice into the thread store — the
    /// cross-device MLS-sync launch path (`docs/goal/behavior/devices.md`
    /// § Cross-device MLS group-state sync, slice 5). Find-or-creates the
    /// channel-keyed thread, adopts the slice's label, and appends its messages
    /// (**incl. the owner's own sent plaintext**, which the nest-ordered log can
    /// never reconstruct on a second device). Returns the thread id so the leg can
    /// bind `channel → thread`; refreshes observers so restored history shows
    /// immediately. Idempotent (the store dedups by message id, so a re-run over a
    /// non-empty store keeps local copies). Delegates to
    /// [`ThreadStore::restore_channel_slice`]. **Rust-only** (the leg's native glue
    /// calls it) — `ChannelHistorySlice` is an at-rest wire type, not a UniFFI type,
    /// so this stays out of the `uniffi::export` block above.
    ///
    /// Also remembers the fetch coordinates the slice carries for its
    /// attachments ([`ChannelHistorySlice::attachment_coordinates`]), so a
    /// restored bubble's first render fetches the bytes instead of rendering
    /// them declared (`conversations.md` § Attachments → *Retention*).
    pub fn restore_channel_slice(&self, slice: &ChannelHistorySlice) -> ThreadId {
        let id = self.threads.restore_channel_slice(slice);
        self.reseed_from_slice(&id, slice);
        id
    }

    /// Fold the history a **room member** re-sealed to this newcomer into the
    /// channel's bound thread (`conversation-rooms.md` § History for joiners →
    /// *What a device accepts*) — the narrow sibling of
    /// [`Self::restore_channel_slice`], which is for the user's own devices.
    /// A member's slice is taken for its **messages** and the derived state
    /// on them, and for nothing else it carries:
    ///
    /// - not its `label` — a governed room's name is a field of the
    ///   owner-signed policy, read off the group context;
    /// - not its `participants` or `anchor_grade_handles`, whatever it records —
    ///   the roster is the group's, and which handle an owner typed is this
    ///   user's own datum (`ChainWitness` dials by it);
    /// - not its attachment coordinates or parked floor deletes.
    ///
    /// So it never goes through [`ThreadStore::restore_channel_slice`], which
    /// adopts all of those. Whether the slice may be folded at all — who
    /// sealed it, when, under which policy, naming which ids — is the rail's
    /// judgment (`backends::fauna_mls::poll_inbound_conv`); this door only
    /// guarantees what a judged slice can reach.
    pub fn restore_joiner_history(&self, thread: &ThreadId, slice: &ChannelHistorySlice) {
        for m in &slice.messages {
            self.threads.append_message(thread, m.clone());
        }
        let messages_only = ChannelHistorySlice {
            attachment_coordinates: Default::default(),
            parked_floor_deletes: Vec::new(),
            ..slice.clone()
        };
        self.reseed_from_slice(thread, &messages_only);
    }

    /// The manager's half of a slice restore, after the store took the
    /// messages: the derived state, the parked records, the attachment
    /// coordinates, the index offer. Shared by both restore doors.
    fn reseed_from_slice(&self, id: &ThreadId, slice: &ChannelHistorySlice) {
        // Re-seed the projection's own memory from the slice
        // (`devices.md` § Cross-device MLS group-state sync — the derived
        // state rides `history/<ch>` precisely because no replay rebuilds it).
        // Without this the restored messages' folded flag and aggregate are
        // live only until the store dedups them away or the first reaction
        // lands; with it, the projection on this device answers exactly what
        // it answered on the writing one.
        // ⚠ **Bounded to the ids this slice actually carries.** Both manager
        // maps are keyed by `MessageId` GLOBALLY, across every thread and
        // channel — but a slice is one channel's, and one of its readers is
        // the history-for-joiners path, where the writer is another room
        // MEMBER rather than the owner's own device
        // (`conversation-rooms.md` § History for joiners). Without this bound
        // a slice naming channel A could tombstone, or paint fabricated pills
        // on, a message of channel B: a forged delete reaching past the room
        // it came from, which is the very thing `DeleteClaim::admits` exists
        // to refuse on the live path. A no-op for an honest slice — the
        // snapshot fills both fields only for messages it carries — so the
        // bound costs the good path nothing and is not left to any caller to
        // remember. (The joiner ingest narrows it further, to messages that
        // device did not already hold first-hand.)
        let carried: std::collections::HashSet<&MessageId> =
            slice.messages.iter().map(|m| &m.message_id).collect();
        if !slice.deleted_messages.is_empty() {
            // A set union: a tombstone is monotone, and this device's own
            // (a delete it made before the restore, in the same session)
            // is as real as the slice's.
            self.deleted.write().unwrap().extend(
                slice
                    .deleted_messages
                    .iter()
                    .filter(|id| carried.contains(id))
                    .cloned(),
            );
        }
        if !slice.reaction_log.is_empty() {
            // Filled, never overwritten — the same posture as the attachment
            // coordinates below, for the same reason: what this device logged
            // itself outranks another device's copy. The only events the log
            // cannot re-derive are this account's OWN reactions, and a device
            // that already holds a log for the message already holds its own;
            // every other member's reaction reaches it through the channel log
            // like any inbound record.
            let mut logs = self.reactions.write().unwrap();
            for (message_id, log) in &slice.reaction_log {
                if carried.contains(message_id) {
                    logs.entry(message_id.clone())
                        .or_insert_with(|| log.clone());
                }
            }
        }
        // The room backend's parked floor delete records: the resumed poll
        // never re-walks them either (the cursor stepped past each one before
        // it was judged), so without the slice's copy every one would stay
        // unjudged for good — its target painted on this device alone, and
        // silently. A union with what this session has already parked, for
        // the tombstone's reason (a parked record may yet be one), bounded
        // like the live set. Keyed by the slice's OWN channel, never a
        // message id: the backend judges each record under that room's
        // anchored chain, and a record's signature binds it to its room, so
        // a slice cannot reach another room's messages through this field
        // any more than through `deleted_messages`.
        if !slice.parked_floor_deletes.is_empty()
            && let Ok(channel) = fauna_mls::types::ChannelId::from_hex(&slice.channel_id_hex)
        {
            let mut parked = self.parked_floor_deletes.write().unwrap();
            crate::store::history::union_parked_floor_deletes(
                parked.entry(channel).or_default(),
                slice.parked_floor_deletes.iter().cloned(),
            );
        }
        // The store starts empty on a relaunch and the resumed poll never
        // re-walks the records that named these attachments, so without the
        // slice's coordinates every one would stay declared for good. Filled,
        // never overwritten: what this device learned itself — its own
        // receive loop or send — outranks another device's copy. Every entry rests on the slice's own
        // channel — the slice names no other.
        if !slice.attachment_coordinates.is_empty()
            && let Ok(channel) = fauna_mls::types::ChannelId::from_hex(&slice.channel_id_hex)
        {
            let mut store = self.attachments.write().unwrap();
            for (blob_hash, blob) in &slice.attachment_coordinates {
                store.remember_if_absent(
                    blob_hash.clone(),
                    AttachmentCoordinates::FaunaMls {
                        channel,
                        blob: blob.clone(),
                    },
                );
            }
        }
        // Content-index catch-up **leg 1** (`content-index.md` § Ingest triggers,
        // v1 → *The Conversation kind's catch-up*): restored history — cross-
        // device history and the owner's own sent plaintext, which the nest-
        // ordered log can never reconstruct on a second device — is offered to
        // the same seam the ingest chokepoints fire. Without this the restore
        // path is the one way a message enters the store unseen by a builder.
        //
        // ⚠ **Inert on every seat that exists today, and deliberately kept
        // anyway.** Both direct-Rust seats and the FFI seat complete the restore
        // *before* the index observer is registered (tui/FFI: `start_receive_loop`
        // awaits `mls_sync_launcher.launch()` in its prologue, two statements
        // above the index launch; linux: `wire_mls_state_sync().await` precedes
        // `start_receive_loop()`), so there is no observer here to take the
        // offer and **leg 2 — the attach-time walk — is what indexes restored
        // history today**. This leg is what keeps that a *scheduling* fact
        // rather than a correctness one: the moment a restore lands after attach
        // (spawning the restore instead of awaiting it is one edit away, and the
        // indefinite transient backoff makes it tempting), this is the leg that
        // stops that history from being silently unsearchable. Overlap with leg
        // 2 costs nothing — the builder's stage-time `(kind, content_id)` guard
        // drops the second offer. `index_launch_ordering_tests.rs` pins the
        // ordering that makes it inert, so a reorder is a red test, not a
        // silent behaviour change.
        for m in &slice.messages {
            // A `ChannelMessage` carries no subject (`ingest_inbound_to_thread`
            // passes the backend's bucket subject, which is `None` for this
            // rail), so the walk matches the live path exactly.
            self.observe_for_index(IndexableKind::Conversation, id, m, None, None);
        }
        self.notify();
    }

    /// Content-index catch-up **leg 2**: offer every fauna-native message
    /// already in the thread store to the index seam — the walk a builder
    /// performs when it attaches (`content-index.md` § Ingest triggers, v1 →
    /// *The Conversation kind's catch-up*: "the builder walks the thread store
    /// through the seam when it attaches (the store is enumerable —
    /// `list_summaries` + `get`)").
    ///
    /// Called once per session by [`ConversationsSession::start_receive_loop`]
    /// immediately after the observer is registered, and a no-op when no
    /// observer is (the `observe_for_index` handle read answers `None`).
    ///
    /// **Why it is the leg that does the work today:** the store at this moment
    /// already holds the restored `history/<ch>` slices (the restore is awaited
    /// before the index launch on every seat — see
    /// [`Self::restore_channel_slice`]), and that history reached it without
    /// passing an observer. Everything the loop folds *after* this point comes
    /// through `ingest_inbound_to_thread` and is seen live.
    ///
    /// **Pass-shaped, hence lease-gated as catch-up staging** — the store
    /// re-presents its whole corpus every launch, so a stood-down seat that
    /// skips this walk loses nothing a later launch cannot redo
    /// (`content-index.md` § Where the index is built). Cheap on the re-walk: a
    /// resumed builder seeds its `(kind, content_id)` guard from the published
    /// corpus, so an already-indexed message is dropped without tokenizing.
    /// The gate this walk meets has had its **first answer**: the launcher
    /// returns from `launch()` only once the lease loop's first step landed
    /// (or its ceiling elapsed), because this walk is offered once per launch
    /// and a closed-by-default gate would withhold it for the whole session
    /// (same §, the launch-walk sub-bullet).
    ///
    /// **Mail threads are skipped, not indexed here.** They live in the same
    /// store, but their catch-up is the mailbox re-page from UID 0 that the
    /// receive loop is about to run, and a doc's kind decides which class key
    /// seals it — offering a mail message as `Conversation` would ask the master
    /// builder to seal it under the wrong class (`content-index.md` § Don't do
    /// these).
    ///
    /// Takes no store lock across the seam call: each thread is cloned out
    /// first, honouring the seam's "the manager holds no lock the sink can
    /// reach" contract (a local-search sink re-enters the manager to resolve
    /// hits).
    pub fn walk_conversations_for_index(&self) {
        if self.index_observer.read().unwrap().is_none() {
            return;
        }
        for summary in self.threads.list_summaries() {
            let Some(detail) = self.threads.get(&summary.thread_id) else {
                continue;
            };
            if detail.rail != Rail::FaunaMls {
                continue;
            }
            for m in &detail.messages {
                self.observe_for_index(
                    IndexableKind::Conversation,
                    &detail.thread_id,
                    m,
                    None,
                    None,
                );
            }
        }
    }

    /// Snapshot a channel-bound thread as a `history/<ch>` replica slice at the
    /// poll's `watermark` (the highest folded seq) — the cross-device MLS-sync
    /// save path (design §5). `None` when the thread is absent. The shared sync
    /// wrapper seals it under the owner's `BackupKey` before upload. Delegates to
    /// [`ThreadStore::snapshot_channel_slice`], then adds what only the manager
    /// knows: the **reaction and delete derived state**
    /// ([`ChannelHistorySlice::deleted_messages`],
    /// [`ChannelHistorySlice::reaction_log`]) and the fetch coordinates of
    /// the thread's attachments from the attachment store
    /// ([`ChannelHistorySlice::attachment_coordinates`]); **Rust-only** (see
    /// [`Self::restore_channel_slice`]).
    pub fn snapshot_channel_slice(
        &self,
        id: &ThreadId,
        channel_id_hex: &str,
        watermark: i64,
    ) -> Option<ChannelHistorySlice> {
        let mut slice = self
            .threads
            .snapshot_channel_slice(id, channel_id_hex, watermark)?;
        // The store captured the messages as they REST; the tombstones and the
        // reaction pills live in manager memory and are projected on top of
        // them on every read. Fold that projection into the slice here —
        // again, the one door both `history/<ch>` writers take (the Rule-3
        // flush and the replica autosave), so neither can write the derived
        // state away (`devices.md` § Durability rules, rule 3: state only this
        // device can produce must reach the replica).
        //
        // Both the fold AND the raw state, for two different readers: the
        // folded flag/aggregate on each message is what an app — and an older
        // binary — renders straight off the slice; `deleted_messages` and
        // `reaction_log` are what this manager's own projection re-seeds
        // from on the other side, and are the only form that survives a
        // restore onto a store that already holds the message (the append
        // dedups and keeps the local copy) or a reaction toggled after the
        // restore (the projection folds the log, and would overwrite an
        // aggregate it did not produce). Both fields say so at their
        // definitions.
        self.project_reactions_and_deletes(&mut slice.messages);
        slice.deleted_messages = slice
            .messages
            .iter()
            .filter(|m| m.deleted)
            .map(|m| m.message_id.clone())
            .collect();
        {
            let logs = self.reactions.read().unwrap();
            for m in &slice.messages {
                if let Some(log) = logs.get(&m.message_id)
                    && !log.is_empty()
                {
                    slice.reaction_log.insert(m.message_id.clone(), log.clone());
                }
            }
        }
        // The room backend's parked floor delete records, at rest — the same
        // door, for the same reason: a record the walk stepped past unjudged
        // is one the resumed poll never meets again, so a slice written
        // without it un-deletes its target on that account for good.
        if let Ok(channel) = fauna_mls::types::ChannelId::from_hex(channel_id_hex) {
            slice.parked_floor_deletes = self.parked_floor_deletes(&channel);
        }
        // The thread store knows messages, not where their attachments' bytes
        // rest; the attachment store does. Stamped here, the one door both
        // `history/<ch>` writers take (the Rule-3 flush and the replica
        // autosave), so neither can write the coordinates away
        // (`conversations.md` § Attachments → *Retention*). Only this channel's
        // own blobs: the slice names no channel per entry, so a restore reads
        // every entry as the slice's.
        if let Ok(channel) = fauna_mls::types::ChannelId::from_hex(channel_id_hex) {
            let store = self.attachments.read().unwrap();
            for message in &slice.messages {
                for attachment in crate::message::attachment_blocks(&message.document) {
                    if let Some(AttachmentCoordinates::FaunaMls {
                        channel: rests_on,
                        blob,
                    }) = store.coordinates(&attachment.blob_hash)
                        && *rests_on == channel
                    {
                        slice
                            .attachment_coordinates
                            .insert(attachment.blob_hash, blob.clone());
                    }
                }
            }
        }
        Some(slice)
    }

    /// Wire the home-nest link-preview seam ([`LinkPreviewRpc`]) — the platform glue
    /// (`fauna-ffi` / `fauna-wasm` / linux `conv_backend`) calls this once, alongside
    /// building the FaunaMls backend's [`ConversationsRpc`](crate::backend::ConversationsRpc),
    /// passing the same `Arc` object (both seams ride the same WS-RPC requester). Left unset on
    /// the receive-only / SMTP-only paths, where [`Self::resolve_link_preview`] then degrades to
    /// no preview.
    pub fn set_link_preview_rpc(&self, rpc: Arc<dyn LinkPreviewRpc>) {
        *self.link_preview_rpc.write().unwrap() = Some(rpc);
    }

    /// Install the session's moderation [`LocalDetectionStore`] handle — called once
    /// by [`ConversationsSession::from_manager`] with the `Arc` the session owns, so
    /// [`Self::ingest_inbound_to_thread`] retains a post-decrypt spam detection for
    /// every incoming social message and the queue reader sees the same store. Set
    /// per session (a fresh login installs its own store), so retained detections
    /// never leak across users even when a Rust-native app reuses one manager
    /// singleton across logins. Left unset on receive-only / test / web managers,
    /// where the classify hook is a no-op.
    pub fn set_local_detection_store(&self, store: Arc<Mutex<LocalDetectionStore>>) {
        *self.local_detections.write().unwrap() = Some(store);
    }

    /// Install the client's content-index sink (`crate::index_sink`) — the
    /// registration app glue performs on a client that builds a local search
    /// index. Every message ingested from here on is offered to the sink at
    /// both inbound chokepoints. A client that never calls this is unaffected;
    /// the seam is optional by design (`content-index.md` § Ingest triggers,
    /// v1 — "an observer seam, not a hard dependency").
    pub fn set_index_observer(&self, observer: Arc<dyn MessageIndexObserver>) {
        *self.index_observer.write().unwrap() = Some(Arc::clone(&observer));
        // The draft corpus reaches the sink from the store itself, because the
        // compose mutators are far too many to be a chokepoint (`DraftStore::
        // set_index_observer`). One registration from app glue still wires both.
        self.drafts.set_index_observer(observer);
    }

    /// Offer one just-ingested message to the content-index sink, if installed.
    /// Reads the handle out of the lock **before** calling, so a sink that
    /// re-enters the manager cannot deadlock against this read guard.
    fn observe_for_index(
        &self,
        kind: IndexableKind,
        thread_id: &ThreadId,
        msg: &MessageSnapshot,
        subject: Option<&str>,
        nest_message_id: Option<&[u8]>,
    ) {
        let Some(observer) = self.index_observer.read().unwrap().clone() else {
            return;
        };
        // Actor-addressed rails carry a real author id; mail senders are email
        // addresses (`TypedAddress::Email`), so the mail slice indexes with
        // `None` and finds its sender through the message itself.
        let sender_actor_id = match &msg.sender {
            TypedAddress::Fauna { actor_id, .. } => Some(actor_id.0),
            _ => None,
        };
        observer.observe_indexable_message(IndexableMessage {
            kind,
            thread_id,
            message_id: &msg.message_id,
            subject,
            body: &msg.body,
            sender_actor_id: sender_actor_id.as_ref(),
            nest_message_id,
            timestamp_ms: msg.timestamp_ms,
            // Only an own copy may supersede an already-indexed doc, which is
            // the collision rule's own asymmetry carried into the index — see
            // `IndexableMessage::is_own`.
            is_own: msg.is_own,
        });
    }

    /// Apply a rename received from a peer (`GroupMeta::NameChanged` over the
    /// channel) as a snapshot-only effect — the receive-side counterpart to the
    /// async [`Self::rename_thread`]. It must NOT fire a backend wire op, or
    /// applying an inbound rename would echo it straight back to the group.
    /// Called by `backends::fauna_mls::poll_inbound_conv`.
    pub fn apply_inbound_rename(&self, id: ThreadId, new_label: String) {
        self.threads.rename(&id, new_label);
        self.notify();
    }

    /// Apply the group's **agreed roster** after a membership commit some other
    /// member authored (`conversation-rooms.md` § The floor roster: the roster
    /// every member renders is the one the MLS group agrees on, never what
    /// this device last did itself) — the receive-side counterpart to
    /// [`Self::remove_participant`], snapshot-only like
    /// [`Self::apply_inbound_rename`]: it fires no wire op.
    ///
    /// **Drop arm.** Drops every Fauna participant whose actor is no longer in
    /// `roster`, **except** one named as a predecessor in `superseded` **whose
    /// successor is itself in `roster`** — the `old` of a recorded identity
    /// succession, whose row [`Self::apply_inbound_succession`] re-points in
    /// place once the statement verifies (which can be parked and re-driven
    /// later; dropping the row first would leave the successor with nothing to
    /// re-point). The successor condition is load-bearing, not incidental: a
    /// `successions` record is self-authored and append-only
    /// (`RoomPolicyExtension::judge_commit`), so an ordinary member can record
    /// one naming themselves as `old` and an invented, never-seated actor as
    /// `new` — retaining unconditionally would let that member pin their own
    /// removal in every other member's rendered roster forever, since the
    /// re-point that would retire the row can never arrive. Non-Fauna rows are untouched.
    ///
    /// **Add arm.** Seats every roster actor this device is not already
    /// rendering, so a member *another* device added joins this list from the
    /// agreed roster alone. Three actors are deliberately not seated:
    ///
    /// - **the local user** — participants are everyone else
    ///   ([`RailBackend::self_address`]); with no self address the arm seats
    ///   nobody rather than render the user as their own participant;
    /// - **an actor already seated**, by [`TypedAddress::same_participant`]
    ///   (actor id, never the handle);
    /// - **an actor a held predecessor already stands in for** — the `new` of a
    ///   `superseded` pair whose `old` is still a row here. That row *is* the
    ///   successor's seat until the verified statement re-points it, so seating
    ///   the successor as a newcomer too would leave the thread rendering one
    ///   member twice, and `apply_inbound_succession` would then re-point the
    ///   predecessor onto a duplicate.
    ///
    /// The seat is **handle-less** — `TypedAddress::Fauna` with an empty handle,
    /// exactly as `backends::fauna_mls::ingest_welcome` seats the members a
    /// Welcome brings, because the engine roster carries actor ids and nothing
    /// else; resolving those ids to handles is one follow-on serving both sites.
    /// It **appends**, so the row order an open editor may be indexing into does
    /// not shift under it.
    ///
    /// `superseded` pairs are `(predecessor, a chain member)` — the caller
    /// expands a multi-hop chain into one pair per identity on it
    /// (`fauna_mls::room_policy::RoomPolicyExtension::successor_chain`), so a
    /// predecessor's row survives while ANY identity on its chain is seated,
    /// not only its terminal one.
    ///
    /// Idempotent; a roster that changes nothing does not even notify.
    pub fn apply_inbound_roster(
        &self,
        id: ThreadId,
        roster: &[ActorId],
        superseded: &[(ActorId, ActorId)],
    ) {
        let mut changed = self
            .threads
            .retain_participants(&id, |p| match p.person_actor_id() {
                Some(actor) => {
                    roster.contains(&actor)
                        || superseded
                            .iter()
                            .any(|(old, new)| *old == actor && roster.contains(new))
                }
                None => true,
            });

        let me = self
            .backends
            .read()
            .unwrap()
            .get(&Rail::FaunaMls)
            .and_then(|b| b.self_address())
            .and_then(|addr| addr.person_actor_id());
        if let Some(me) = me {
            let seated: Vec<ActorId> = self
                .threads
                .get(&id)
                .map(|d| {
                    d.participants
                        .iter()
                        .filter_map(|p| p.person_actor_id())
                        .collect()
                })
                .unwrap_or_default();
            // Successors a still-seated predecessor is already standing in for.
            let stood_in_for: Vec<ActorId> = superseded
                .iter()
                .filter(|(old, _)| seated.contains(old))
                .map(|(_, new)| *new)
                .collect();
            for actor in roster {
                if *actor == me || seated.contains(actor) || stood_in_for.contains(actor) {
                    continue;
                }
                self.threads
                    .add_participant_to(&id, self.seat_address_for(*actor));
                changed = true;
            }
        }

        if changed {
            // A seat copied its name off another thread; carry that thread's
            // provenance with it (`ThreadStore::inherit_anchor_grade`), so a
            // person the owner typed elsewhere stays anchor-grade here and a
            // room-home name stays display only.
            self.threads.inherit_anchor_grade(&id);
            self.notify();
        }
    }

    /// An inbound commit advanced `id`'s epoch: tick the observers, whatever
    /// else it changed.
    ///
    /// The room projection — `ThreadDetail::room` and the role-gated
    /// capabilities it overlays — is derived from the agreed group context on
    /// every read ([`Self::thread_detail`]), so a commit that moved only the
    /// policy (an appointment, a demotion, a hand-over, a rule change) changes
    /// what every app paints while changing no state this manager holds: the
    /// label refresh and [`Self::apply_inbound_roster`] each tick only on their
    /// own change. An event-driven painter repaints only on a tick, so without
    /// this one a seat whose thread is already open keeps painting the room as
    /// it stood before the commit — the owner of yesterday's policy door still
    /// live after the hand-over landed (`conversation-rooms.md` § Roles and
    /// authorization; `ui/conversations.md` § Architectural rules 5). A commit
    /// that also moved the roster or the name ticks twice; an extra repaint of
    /// an unchanged frame is the cheap side of that trade.
    pub fn room_projection_moved(&self, id: &ThreadId) {
        tracing::debug!(thread = %id.0, "an inbound commit moved the room's group context");
        self.notify();
    }

    /// Apply a **verified** in-group succession as a thread effect
    /// (`identity-succession.md` § Propagation → *MLS groups*: members treat
    /// the add+remove pair as one logical operation): re-point the thread's
    /// participant row old→new in place — same list position, handle kept (the
    /// nest moved it to the successor inside the succession transaction) —
    /// rather than letting the pair render as "a stranger arrived, a member
    /// left". Historical messages keep their old-id attribution (they were
    /// genuinely authored by that key). Idempotent: a re-delivered statement
    /// finds no participant bearing `old` and is a no-op. Called by
    /// `backends::fauna_mls::poll_inbound_conv` **only after** its
    /// [`crate::backend::SuccessionWitness`] verified the statement — this
    /// method trusts its caller and must never be handed an unverified claim.
    ///
    /// The same verdict folds the person's private contact overlay forward
    /// (`contacts.md` § The private overlay → *When a person's identity
    /// succeeds*): it is recorded for the projection-load reconcile, which
    /// runs at once.
    pub fn apply_inbound_succession(&self, id: ThreadId, old: &ActorId, new: ActorId) {
        self.threads.repoint_participant(&id, old, new);
        if old != &new {
            self.contact_overlays
                .write()
                .unwrap()
                .successions
                .insert(*old, new);
            self.reconcile_overlay_folds();
        }
        self.notify();
    }

    /// The Fauna participants of `id` this device is rendering as an **elided
    /// actor id** — seated members no handle has resolved for yet. The read
    /// half of the id-keyed handle read (`conversation-rooms.md`
    /// § Implementation status today, the roster bullet): its answer is
    /// exactly the set worth spending a network read on, so a thread whose
    /// members all have names costs nothing.
    ///
    /// Empty for a thread this device does not hold, and for one whose
    /// participants are all named or all non-Fauna.
    pub fn nameless_participants(&self, id: &ThreadId) -> Vec<ActorId> {
        self.threads
            .get(id)
            .map(|d| {
                d.participants
                    .iter()
                    .filter(|p| p.person_handle().is_none())
                    .filter_map(|p| p.person_actor_id())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Name the seated participants an id-keyed handle read resolved, in place
    /// — the write half of [`Self::nameless_participants`], and the network
    /// counterpart of [`Self::seat_address_for`]'s device-local scan.
    ///
    /// Re-points the *existing* row rather than seating a newcomer, the way
    /// [`Self::apply_inbound_succession`] does, so the list position an open
    /// editor is indexing into does not shift and `participant_displays`
    /// follows ([`crate::store::threads::ThreadStore::name_participants`]
    /// carries the argument). Only ever adds a name, never overwrites one.
    /// Notifies only when something actually changed, so a re-read that
    /// resolves nothing new is silent.
    ///
    /// ⚠ **Display only, and never an identity claim — nor a succession
    /// anchor.** A handle read this way says what to show; membership still
    /// keys on the actor id ([`TypedAddress::same_participant`]) — the same
    /// rule [`Self::handle_for_person`] carries for the device-local read.
    /// And it is the room home's answer, so it never becomes the domain a
    /// succession's tier 2 dials: [`Self::anchor_grade_handle_for`] answers
    /// only for a handle the owner's own gesture put on a row
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*).
    pub fn apply_resolved_handles(&self, id: ThreadId, resolved: &[(ActorId, String)]) {
        if self.threads.name_participants(&id, resolved) {
            self.notify();
        }
    }

    /// The handle **the owner's own gesture** named for `actor` on some thread
    /// this device holds — a recipient typed and accepted at compose, a member
    /// the owner added — or `None` when every name this device renders for
    /// them came from a room home or was copied off such a row. The one
    /// participant-handle provenance a succession's tier 2 may dial
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*; the witness reads it through
    /// `SuccessionAnchors::known_handle`). Rust-only: provenance is nothing
    /// an app renders. In-memory scan, no I/O.
    pub fn anchor_grade_handle_for(&self, actor: &ActorId) -> Option<String> {
        self.threads.anchor_grade_handle_for(actor)
    }

    /// Tell the **outgoing** owner that the hand-over it offered can no longer
    /// be completed (`conversation-rooms.md` § Roles and authorization →
    /// *Ownership transfer*: "an offer the room has moved past … is refused by
    /// every member and dropped by the device holding it; the owner offers
    /// again"). Until this existed the owner learned that only from the roles
    /// not changing: the offer is parked on the *incoming* owner's device
    /// (`FaunaMlsBackend::park_ownership_offer` returns unless the offered
    /// policy names this identity), so the seat that must act on the refusal
    /// is the one seat holding no record of it.
    ///
    /// Called from the offering device's own commit fold
    /// (`backends::fauna_mls::notice_superseded_own_offer`) once the agreed
    /// policy has moved to or past the offered version with this identity
    /// still the owner — i.e. the room advanced without the transfer.
    ///
    /// It writes the ordinary page error
    /// (`conversations.unified.error_set_room_policy`), which is the right slot
    /// precisely because a superseded offer is a **one-time event, not a
    /// standing refusal**: the clear-on-entry contract that slot carries
    /// (`../ui/conversations.md` § Errors & edge cases) would mask a standing
    /// truth, and is exactly what should retire this notice the moment the
    /// owner's next gesture supersedes it in turn.
    pub fn apply_superseded_ownership_offer(&self) {
        self.set_room_policy_error(
            fauna_i18n::strings::error::send::ROOM_TRANSFER_SUPERSEDED.to_string(),
        );
    }

    /// Apply an inbound reaction (any member may react). Appends to the target's
    /// reaction log; thread_detail folds it onto the message (out-of-order safe —
    /// the log holds it until the target message arrives). Called by poll_inbound_conv.
    ///
    /// `sent_at_ms` is the **author's own stamp**, in milliseconds: the
    /// community class's signed `sent_at_ms`, the end-to-end class's
    /// `ChannelMessage.timestamp` divided down from microseconds. It is what
    /// [`fold_reactions`] orders by, so a rail that could not supply it would
    /// hand this reactor's ops back to plain log order — which on the
    /// community class is replayable (`crate::reactions::StampedReactionEvent`).
    /// Both live rails supply it, and it is required: the stamp-less shape was
    /// retired by the compat-remnant sweep (`version-compatibility.md`
    /// § Dimension 2, program 4).
    ///
    /// The append is unconditional and stays so: the fold reads the SET of ops,
    /// so a duplicate — a re-walk, an overlapping re-poll, or a hostile replay
    /// — costs a log entry and changes no outcome.
    pub fn apply_inbound_reaction(
        &self,
        target: MessageId,
        reactor: ActorId,
        emoji: String,
        op: ReactionOp,
        sent_at_ms: i64,
    ) {
        self.reactions
            .write()
            .unwrap()
            .entry(target)
            .or_default()
            .push(StampedReactionEvent::new(reactor, emoji, op, sent_at_ms));
        self.notify();
    }

    /// Record an inbound cooperative-delete CLAIM. thread_detail honors it only if
    /// `claimed_sender` IS the target's own sender (forged-delete drop). Out-of-order
    /// safe — the claim is validated at projection once the target arrives.
    pub fn apply_inbound_delete(&self, target: MessageId, claimed_sender: ActorId) {
        self.apply_inbound_delete_claim(
            target,
            DeleteClaim {
                claimant: claimed_sender,
                delete_seq: None,
                role: None,
            },
        );
    }

    /// [`Self::apply_inbound_delete`] with the rail's whole verdict: the
    /// delete's own log position and the role its authenticated author held
    /// under the policy it was made under. Idempotent — a re-walk that re-feeds
    /// the same claim records it once.
    pub fn apply_inbound_delete_claim(&self, target: MessageId, claim: DeleteClaim) {
        {
            let mut claims = self.delete_claims.write().unwrap();
            let held = claims.entry(target).or_default();
            if held.contains(&claim) {
                return;
            }
            held.push(claim);
        }
        self.notify();
    }

    /// Whether `thread` itself already holds a message with `id` — what the
    /// MLS inbound poll asks before it skips a record's decrypt, and what the
    /// joiner's history fold asks before it takes a slice's verdict on a
    /// message. The skip keeps a Rule-2 heal re-walk or an overlapping re-poll
    /// cheap (the store's own dedup would drop the append anyway).
    /// **Thread-local on purpose — there is deliberately no store-wide
    /// `has_message`**: a record's id is minted by its own channel's log, so
    /// only that channel's thread can already hold it, and an id some other
    /// thread was handed must never make the walk step over a record it has
    /// not read
    /// (`conversation-rooms.md` § History for joiners → *What a device
    /// accepts*).
    pub fn thread_holds_message(&self, thread: &ThreadId, id: &MessageId) -> bool {
        self.threads.thread_holds_message(thread, id)
    }

    /// Ingest a decrypted inbound message into a specific, already-resolved
    /// thread — the channel-keyed routing path for the fauna-native MLS rail.
    /// FaunaMls threads are identified by their MLS channel (not by participants
    /// / subject like [`Self::ingest_inbound`]): two channels between the same
    /// actors are distinct threads, and a `ChannelMessage` carries no subject.
    /// The caller (`backends::fauna_mls::poll_inbound_conv`) resolves the
    /// channel→thread binding; this shapes the message via the rail backend's
    /// `bucket_inbound` and appends it. Native-only (driver-facing), like
    /// [`Self::send`].
    pub fn ingest_inbound_to_thread(
        &self,
        thread_id: ThreadId,
        msg: RailInboundMessage,
    ) -> Result<(), BackendError> {
        self.ingest_inbound_with_server_labels(thread_id, msg, &[])
    }

    /// [`Self::ingest_inbound_to_thread`] for a record its nest served
    /// **verdicts** beside — a community room's message, which its home nest
    /// labelled with the labelers the room names (`conversation-rooms.md`
    /// § The three classes → *What the home nest does with its read*,
    /// purpose 2). They merge into the device's own post-decrypt labels by the
    /// superset rule — a server verdict wins its category, a local detection the
    /// server did not make stays and never leaves the device
    /// ([`fauna_core::content_category::merge_server_labels`]) — so the badge
    /// every app paints off [`MessageSnapshot::labels`] shows both.
    ///
    /// A message already on the device when its record is walked (the sender's
    /// own echo) is not re-ingested, so it keeps the device's labels.
    pub fn ingest_inbound_with_server_labels(
        &self,
        thread_id: ThreadId,
        msg: RailInboundMessage,
        server_labels: &[fauna_core::content_category::ContentLabelEntry],
    ) -> Result<(), BackendError> {
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(&msg.rail)
            .cloned()
            .ok_or(BackendError::NotSupported)?;
        let mut bucket = backend.bucket_inbound(msg, None)?;
        // Post-decrypt moderation classify hook (before the message is moved into
        // the thread store) — the encrypted-mode social-content signal. Also
        // stamps `bucket.message.labels` from the same classify pass
        // (`moderation.md` § Per-row badge data path) so the badge renders from
        // live data, not just the moderation queue.
        self.observe_local_detection(&mut bucket.message);
        if !server_labels.is_empty() {
            bucket.message.labels = fauna_core::content_category::merge_server_labels(
                server_labels,
                &bucket.message.labels,
            );
        }
        // The conversations-rail arm of the same content-index seam. Wired now
        // so the seam is uniform across both chokepoints; the S3 mail-slice
        // sink ignores `Conversation` docs, and rollout slice S4 turns them on
        // (master-key slice) with no change here.
        self.observe_for_index(
            IndexableKind::Conversation,
            &thread_id,
            &bucket.message,
            bucket.subject.as_deref(),
            None,
        );
        self.threads.append_message(&thread_id, bucket.message);
        self.notify();
        Ok(())
    }

    /// Classify one just-decrypted **incoming** social message: stamp its
    /// per-row [`MessageSnapshot::labels`] (`moderation.md` § Per-row badge data
    /// path) and retain a spam detection in the session's [`LocalDetectionStore`]
    /// — the post-decrypt classify hook mandated by
    /// `docs/goal/behavior/moderation.md` § Layout & flow (the queue is the union
    /// of the server obligation rows and the client's own post-decrypt local
    /// detections) and placed here per
    /// `docs/goal/architecture/content-scoring.md` § "The two plaintext positions in
    /// encrypted mode" → position 2 (the user's client, post-decrypt): in encrypted
    /// mode the nest holds only ciphertext and runs **no** content scorer, so the
    /// client is the only place a fauna-native (MLS-sealed) conversation message can
    /// be classified. Runs the shared `fauna_core::text_heuristic::classify_text`
    /// (the same heuristic the plaintext-mode nest runs server-side) once and feeds
    /// both consumers: the badge gets every classified category regardless of
    /// whether a store is installed (presentation, not the moderation queue); the
    /// store additionally applies its own spam-only category + confidence gate and
    /// bounded most-recent-first retention (a no-op when no store is installed —
    /// receive-only / test / web-pre-slice-5 managers).
    fn observe_local_detection(&self, msg: &mut MessageSnapshot) {
        // Own (multi-device self-echo) content is never a received-spam flag —
        // `poll_inbound_conv` already skips own messages at MLS-decrypt, so this is
        // defence in depth (and correct for any future non-MLS caller).
        if msg.is_own {
            return;
        }
        // Classifier confidence is `f64` in 0.0–1.0; the wire-aligned scaling is
        // per-mille (`> 300` ≡ web's `> 0.3`).
        let results = fauna_core::text_heuristic::classify_text(&msg.body);
        msg.labels = results
            .iter()
            .map(|r| fauna_core::content_category::ContentLabelEntry {
                category: r.category.clone(),
                confidence_per_mille: (r.confidence * 1000.0).round().clamp(0.0, 1000.0) as u16,
            })
            .collect();
        let Some(store) = self.local_detections.read().unwrap().clone() else {
            return;
        };
        // The store's `observe` keeps only the strongest qualifying `spam` label
        // (category + gate applied there), so a clean message contributes nothing.
        let labels: Vec<DetectionLabel> = results
            .into_iter()
            .map(|r| DetectionLabel {
                category: r.category,
                confidence_per_mille: (r.confidence * 1000.0).round() as u16,
            })
            .collect();
        if labels.is_empty() {
            return;
        }
        // `content_id` = the message id (`conv:<channel>:<seq>`), the same ref a
        // server `ObligationAction` would key on, so `merge_queue` dedupes across the
        // two sources cleanly. `timestamp_ms` → microseconds to match
        // `ObligationAction::timestamp` (so the merged queue sorts consistently).
        store.lock().unwrap().observe(
            msg.message_id.0.clone(),
            "message",
            msg.timestamp_ms.saturating_mul(1000),
            &labels,
        );
    }

    /// The decrypted plaintext body of one retained message, by the message-id
    /// string a moderation queue row carries as its `content_id` (a local
    /// detection keys on `MessageSnapshot.message_id` —
    /// [`Self::observe_local_detection`]). The train-correction surface reads
    /// this to feed the ham correction to the **client-side** tier-1 spam-model
    /// write (`fauna_client_mail_settings::MailSettingsMachine::apply_spam_model_write`)
    /// with the same post-decrypt text the classifier saw — text only the client
    /// holds (MLS-sealed at rest, `moderation.md` § Layout & flow). `None` once
    /// the message has aged out of the store (the correction then just clears
    /// the flag, exactly as before the client write path existed).
    pub fn message_body(&self, message_id: &str) -> Option<String> {
        self.threads
            .message_body(&crate::message::MessageId(message_id.to_string()))
    }

    /// Locate a message by id: the thread that holds it plus its plaintext body.
    ///
    /// The local search arm projects every sealed-index hit through this
    /// (`fauna_client_index::local_search`) — the index stores postings only, so
    /// the snippet renders from the body, and `SearchNav::Mail` needs the
    /// holding thread. `None` for a message this device's store does not hold,
    /// which the projection treats as "not renderable", never as an error.
    pub fn locate_message(&self, message_id: &str) -> Option<(ThreadId, String)> {
        self.threads
            .locate_message(&crate::message::MessageId(message_id.to_string()))
    }

    /// Resolve an indexed **draft** by its content id — the query-side half of
    /// the drafts arm, mirroring [`Self::locate_message`].
    ///
    /// Answers from the live [`DraftStore`], never from the index, which is what
    /// makes a hit on a draft the user has since discarded resolve to `None` and
    /// be dropped (`ui/search.md` § State & data shape — *An unresolvable local
    /// hit is DROPPED*) rather than opening an empty composer. The returned
    /// thread id is `None` for the new-thread compose slot.
    pub fn locate_draft(&self, content_id: &str) -> Option<(Option<ThreadId>, String)> {
        if content_id == crate::index_sink::NEW_THREAD_DRAFT_ID {
            let compose = self.drafts.new_thread()?;
            return Some((None, compose.body_draft));
        }
        let thread_id = ThreadId(content_id.to_string());
        let compose = self.drafts.get(&thread_id);
        // `get` synthesizes a default for an unknown thread, so "no draft" and
        // "a draft with no text" arrive here identically — both mean there is
        // nothing to render or navigate to.
        if compose.body_draft.trim().is_empty() {
            return None;
        }
        Some((Some(thread_id), compose.body_draft))
    }

    /// Materialize (or look up) the fauna-native MLS thread for a channel — the
    /// receiver-side counterpart to send-bootstrap, called by
    /// `backends::fauna_mls::ingest_welcome` after joining a group from a
    /// Welcome. Channel-keyed ([`ThreadKey::Channel`]) so two groups with the
    /// same membership stay distinct, and idempotent: a re-delivered Welcome
    /// returns the existing thread. Native-only (driver-facing), like
    /// [`Self::ingest_inbound_to_thread`].
    pub fn materialize_conv_thread(
        &self,
        channel_id_hex: String,
        participants: Vec<TypedAddress>,
    ) -> ThreadId {
        let flavor = if participants.len() <= 1 {
            ThreadFlavor::OneToOne
        } else {
            ThreadFlavor::MlsGroup
        };
        let id = self.threads.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex,
            },
            participants,
            flavor,
            None,
        );
        // A Welcome roster carries no handles; whatever name a row has was
        // copied off another thread at the seat, and its provenance comes with
        // it (`ThreadStore::inherit_anchor_grade`). Nothing here is ever
        // marked anchor-grade outright: the group's delivered values are
        // chosen by the adder or the channel host.
        self.threads.inherit_anchor_grade(&id);
        self.notify();
        id
    }
}

// ── Async client surface ──────────────────────────────────────────────────
//
// The single-method-per-action surface clients drive from
// `docs/goal/ui/conversations.md` § User actions: outbound send, membership
// (add / remove / rename), recipient resolution, and login-time key-package
// publication. Exported over UniFFI with `async_runtime = "tokio"` (Track E) so
// the native FFI clients (windows/macos/ios/android) `.await` them through the
// generated bindings exactly as the linux app awaits them in-process; the
// wasm SPA reaches the same methods through the `fauna-wasm` `future_to_promise`
// wrappers (Track E2). All MLS crypto stays behind the rail backends — only the
// snapshot-shaped result crosses the FFI, never a `ChannelId`
// (`conversations.md` § Architectural rules #2). On wasm the `uniffi` feature is
// off, so this is a plain `impl` driven by the wasm wrappers.
#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl ConversationsManager {
    /// Resolve link-preview metadata (render-model.md § D4) for the bare `url` carried by a
    /// `RenderBlock::LinkPreview { Resolving }` block in a loaded bubble — the conversations
    /// twin of `FeedManager::resolve_link_preview`. Calls `fauna.linkpreview.resolve` once per
    /// URL (the result is cached in [`resolved_previews`](Self::resolved_previews) keyed by
    /// URL), maps the reply onto `PreviewState`, and notifies so the next
    /// [`thread_detail`](Self::thread_detail) folds the matching block `Resolved`/`Failed`. A
    /// transport error **or** an explicit `Failed` both collapse to the render model's terminal
    /// `Failed` (the bubble falls back to the plain inline link, § D4). Idempotent: a repeat
    /// call for an already-resolved URL is a no-op with NO further notify — render-loop-safe, so
    /// a non-fire-once client observer re-calling from a notify-driven re-render gets a no-op. A
    /// no-op too when no [`LinkPreviewRpc`] is wired (receive-only / SMTP-only).
    pub async fn resolve_link_preview(&self, url: String) {
        // Clone the seam Arc out before the await (never hold the lock across it).
        let rpc = self.link_preview_rpc.read().unwrap().clone();
        let notified =
            resolve_link_preview_cached(&self.resolved_previews, url.clone(), async move {
                let rpc = rpc?;
                Some(match rpc.link_preview_resolve(url).await {
                    Ok(LinkPreviewResolution::Resolved {
                        title,
                        description,
                        image_hash,
                    }) => PreviewState::Resolved {
                        title,
                        description,
                        image_hash,
                        // Blocked-by-default at resolution (render-model.md § D4): the
                        // thread_detail reveal walk flips this `true` once the user reveals the
                        // message's remote content.
                        revealed: false,
                    },
                    Ok(LinkPreviewResolution::Failed) | Err(_) => PreviewState::Failed,
                })
            })
            .await;
        if notified {
            self.notify();
        }
    }

    /// Send the per-thread compose draft for an existing thread, routing
    /// through the thread's rail backend
    /// (`docs/goal/ui/conversations.md` § User actions:
    /// `dm-send-button → manager.send(thread_id)`). On success appends the
    /// sent message and clears the draft; on failure stamps
    /// `ComposeState.send_state = Failed { reason }` and returns the error.
    pub async fn send(&self, id: ThreadId) -> Result<(), BackendError> {
        // A send supersedes the last membership/label gesture's error, so a
        // stale page error can never outlive the gesture it described (and can
        // never mask this send's own `send_state` failure on a client that
        // renders one `error-message` for both).
        self.clear_page_error();
        let Some(detail) = self.thread_detail(id.clone()) else {
            return Err(BackendError::Internal("no such thread".to_string()));
        };
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(&detail.rail)
            .cloned()
            .ok_or(BackendError::NotSupported)?;
        let compose = self.drafts.get(&id);
        // Resolve the light attachment drafts to bytes from the store, for the
        // backend to put on the wire (SMTP inlines them as MIME parts). A draft
        // whose bytes this device does not hold refuses here, before anything
        // reaches the backend, into the same failure slot a backend rejection
        // takes.
        let attachments = match self.resolve_attachments(&compose) {
            Ok(attachments) => attachments,
            Err(e) => {
                tracing::warn!("send refused before the backend: {e:?}");
                self.set_send_state(&id, SendState::failed(e.user_detail()));
                return Err(e);
            }
        };

        self.set_send_state(&id, SendState::Sending);

        match backend.send(&detail, &compose, &attachments).await {
            Ok(outcome) => {
                // The sender's own "Sent" copy must render like the peers'
                // received copy: on a markdown-capable rail (FaunaMls and —
                // since html-mail.md — SMTP) the body is markdown, so flag it
                // `Markdown` and the bubble renders `**bold**` formatted
                // instead of as raw source (`conversations.md` § Layout —
                // `dm-message-text` rendered per `body_format`). Non-markdown
                // rails (a bridge) stay PlainText.
                let body_format = if detail.capabilities.supports_markdown {
                    BodyFormat::Markdown
                } else {
                    BodyFormat::PlainText
                };
                // The sender's own copy renders the attachments it just sent —
                // the resolved set the backend received, so echo and wire agree;
                // the bubble loads the real file via `attachment_bytes`.
                let attachments = Self::sent_echo_attachments(&attachments);
                let msg = MessageSnapshot {
                    message_id: outcome.message_id,
                    sender: outcome.sender,
                    sender_display: String::new(),
                    body: compose.body_draft.clone(),
                    // The complete render document the shells paint (body +
                    // attachment blocks), produced once here from the same
                    // body+format+attachments (render-model.md § D1/D2).
                    document: crate::message::document_for_message(
                        &compose.body_draft,
                        body_format,
                        &attachments,
                    ),
                    timestamp_ms: outcome.timestamp_ms,
                    subject_line: None,
                    badges: MessageBadges::default(),
                    reply_to: compose.reply_to.clone(),
                    reactions: vec![],
                    deleted: false,
                    is_own: true,
                    // A message the local user just sent is never taken down.
                    legal_takedown_ref: None,
                    // Own content is never classified as received-spam (mirrors
                    // `observe_local_detection`'s is_own skip).
                    labels: vec![],
                    // Carried from the backend's outcome. A `conv` scope is a
                    // member scope, so an own message on it is browse content
                    // under T1 exactly like a received one — and this echo is
                    // the only copy that will ever carry the ref.
                    plane_ref: outcome.plane_ref,
                    can_delete: false,
                };
                // The own copy reaches the content index NOW — the receive poll
                // never re-presents a sender's own record on the device that
                // sent it, so without this a sent message stayed unsearchable
                // until the next launch's store walk (`content-index-ingest.md`
                // § Ingest triggers, v1 → the own-send ruling). Offered exactly
                // as that walk would (no subject, no record id), so its re-offer
                // next launch is dropped by the builder's guard. Mail waits for
                // its Sent copy instead, which carries the record id a mail doc
                // is identified by (the `AlreadyOwn` arm of
                // `ingest_inbound_identified`).
                if detail.rail != Rail::Smtp {
                    self.observe_for_index(index_kind_for_rail(detail.rail), &id, &msg, None, None);
                }
                self.threads.append_message(&id, msg);
                // Where the attachments just sent rest (FaunaMls: the sealed
                // blobs on the room's home nest). No receive loop will ever
                // tell this device — a sender never walks its own record back
                // (`SendOutcome::attachment_coordinates`) — so without this an
                // own attachment the budget evicts renders declared, and the
                // Rule-3 flush below writes the `history/<ch>` slice without
                // them (`conversations.md` § Attachments → *Retention*).
                for (blob_hash, coordinates) in outcome.attachment_coordinates {
                    self.remember_attachment_coordinates(blob_hash, coordinates);
                }
                // Sent: clear the draft body/subject/reply, back to Idle.
                self.drafts.set(
                    id.clone(),
                    ComposeState {
                        send_state: SendState::Idle,
                        ..Default::default()
                    },
                );
                // Sender-side channel-keying: once a FaunaMls group has
                // bootstrapped and bound its channel, re-key the thread from the
                // participant key `send_new_thread` created to channel-keyed,
                // matching the receiver (`materialize_conv_thread`). Two
                // sender-initiated groups with identical membership then stay
                // distinct instead of colliding on the participant key.
                if detail.rail == Rail::FaunaMls
                    && let Some(channel_hex) = backend.channel_binding_hex(&id)
                {
                    self.threads.rekey_to_channel(&id, channel_hex);
                }
                self.notify();
                // Rule 3 (durable-before-done): the send action completes only
                // once the just-appended own message — which a sender can never
                // MLS-decrypt off the log — is durably persisted.
                self.persist_thread_history(&id).await;
                Ok(())
            }
            Err(e) => {
                // The slot renders only product statements (`user_detail`, the
                // send-slot taxonomy — conversations.md § Errors & edge cases);
                // the raw error, diagnostics included, goes to the log here.
                tracing::warn!("send failed: {e:?}");
                self.set_send_state(&id, SendState::failed(e.user_detail()));
                Err(e)
            }
        }
    }

    /// Materialize the new-thread compose into a real thread, then send it
    /// (`dm-send-button → manager.send_new_thread()` for new-thread compose).
    /// Returns the new thread id, or `None` when there is no active
    /// new-thread compose / no committed recipient chip. On send failure the
    /// thread is left materialized with a `Failed` draft (the user can retry).
    pub async fn send_new_thread(&self) -> Result<Option<ThreadId>, BackendError> {
        // Flush a typed-but-uncommitted recipient. A user who types an address
        // and clicks Send without first pressing Enter / clicking a suggestion
        // would otherwise hit the `chips.is_empty()` bail below and silently
        // no-op (`Ok(None)`) — the live "I click Send and nothing happens" bug.
        // Mirror the picker's on-accept path exactly (resolve — which promotes a
        // Fauna handle to a real actor — then commit the chip) so the primary
        // Send action commits the pending recipient just like Enter does.
        // `accept_new_thread_chip` clears `raw_input`, so this is a no-op once a
        // chip is already committed.
        let has_pending_recipient = self
            .drafts
            .new_thread()
            .and_then(|c| c.recipient_picker)
            .is_some_and(|p| p.chips.is_empty() && !p.raw_input.trim().is_empty());
        if has_pending_recipient {
            self.resolve_recipient().await;
            self.accept_current_recipient_chip();
        }

        let Some(compose) = self.drafts.new_thread() else {
            return Ok(None);
        };
        let Some(picker) = compose.recipient_picker.clone() else {
            return Ok(None);
        };
        if picker.chips.is_empty() {
            return Ok(None);
        }
        let participants = picker.chips.clone();
        // A chip of a kind this build does not name (one carried in from a
        // newer device's drafts blob) has no rail to send on: refuse rather
        // than guess one.
        let Some(rail) = participants[0].rail() else {
            return Err(BackendError::NotSupported);
        };

        // **A community room is founded, not bootstrapped.** The home-nest
        // choice seats the nest, so the room is one the nest reads
        // (`conversation-rooms.md` § The three classes) — born by the room
        // ceremony rather than as an MLS group, which is why it forks before
        // any thread routing is decided. A bridge-ridden recipient keeps the
        // room transport-only whatever the toggle says (the class rule is
        // ordered), so it takes the ordinary path.
        if picker.include_home_nest
            && crate::room::prospective_room_class(&participants, true)
                == Some(crate::room::RoomClass::Community)
        {
            return self.found_room_from_compose(compose, participants).await;
        }

        let normalized = compose
            .subject_draft
            .as_deref()
            .map(normalize_subject)
            .filter(|s| !s.is_empty());
        let (key, flavor, label_override) = match normalized {
            Some(subject) => (
                ThreadKey::SubjectKeyed {
                    rail,
                    participants: participants.clone(),
                    subject,
                },
                ThreadFlavor::SubjectKeyed,
                compose.subject_draft.clone(),
            ),
            None => (
                ThreadKey::Participants {
                    rail,
                    participants: participants.clone(),
                },
                if participants.len() == 1 {
                    ThreadFlavor::OneToOne
                } else {
                    ThreadFlavor::SubjectKeyed
                },
                None,
            ),
        };
        let owner_named: Vec<ActorId> = participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect();
        let id = self
            .threads
            .find_or_create(key, participants, flavor, label_override);
        // Every recipient here was resolved and accepted by the owner: the
        // one handle provenance a succession's tier 2 may dial
        // (`ThreadStore::mark_anchor_grade`).
        self.threads.mark_anchor_grade(&id, owner_named);

        // Move the compose body/subject/attachments onto the new thread's
        // per-thread draft, close the new-thread compose, and select the thread.
        // Attachments carry over so the subsequent `send(id)` inlines them (the
        // staged bytes are already in the store, keyed by the same `blob_hash`).
        self.drafts.set(
            id.clone(),
            ComposeState {
                body_draft: compose.body_draft.clone(),
                subject_draft: compose.subject_draft.clone(),
                attachments: compose.attachments.clone(),
                ..Default::default()
            },
        );
        self.drafts.set_new_thread(None);
        *self.new_thread_active.write().unwrap() = false;
        *self.selected.write().unwrap() = Some(id.clone());
        self.notify();

        self.send(id.clone()).await?;
        Ok(Some(id))
    }

    // ── Membership wire-drivers (mirror `send`) ─────────────────────
    //
    // The client-facing single-method-per-action surface from
    // `docs/goal/ui/conversations.md` § User actions: `confirm_add_participant`,
    // `rename_thread`, `remove_participant`. Each does the snapshot mutation
    // **and**, for an already-bound FaunaMls group, the backend MLS wire op
    // (Commit + Welcome / encrypted `NameChanged`). The snapshot mutation runs
    // first and unconditionally, so the optimistic UI update stands even when
    // the wire op fails or the thread isn't bound to a channel yet (e.g. a test
    // fixture or a FaunaMls backend not registered on this client). The
    // receive-side counterparts that must NOT fire wire ops are
    // `apply_inbound_rename` and (membership) `poll_inbound_conv`'s commit arm.

    /// Stamp [`ConversationsSnapshot::error`] and emit, so the failure reaches
    /// `error-message` on the very tick that produced it. Emitting here — at the
    /// *event* — rather than leaving it for a caller's later `notify` is what
    /// makes the surface hold for `remove_participant`/`rename_thread`, whose
    /// own `notify` fires **before** their wire op runs.
    fn set_page_error(&self, error: LocalizedText) {
        // Producer-side log at the event, not the paint (`observability.md`);
        // `log_line` is redaction-safe, so no per-platform i18n lookup is needed.
        tracing::warn!("conversations page error: {}", error.log_line());
        *self.page_error.write().unwrap() = Some(error);
        self.notify();
    }

    /// The page error the last membership/label gesture reported, as a plain
    /// `key: message` diagnostic — for a **test agent**, not for paint.
    ///
    /// [`Self::confirm_add_participant`], [`Self::remove_participant`] and
    /// [`Self::rename_thread`] are UI gestures: they report a failed wire op by
    /// stamping the page error and return `()` / `Option`, never a `Result`. An
    /// agent command that awaits one of them therefore has **no return value to
    /// check** — it must read this back, or it acks success for a wire op that
    /// failed, which is the convention-11 swallow (`e2e-conventions.md` §
    /// convention 11).
    ///
    /// `args["message"]` is what the *user* sees — the producers stamp it
    /// through `BackendError::user_detail`, so a product statement (a nest
    /// refusal, a version-mismatch sentence) reaches a test author verbatim,
    /// while a **diagnostic** reads as the generic sentence and its detail is in
    /// the log line beside the stamp. That is the send-slot taxonomy applying to
    /// this element's other producer, not a gap: a diagnostic is by definition
    /// not renderable, so a reader that needs the raw text needs the log.
    pub fn page_error_diagnostic(&self) -> Option<String> {
        let error = self.page_error.read().unwrap().clone()?;
        Some(match error.args.get("message") {
            Some(message) if !message.is_empty() => format!("{}: {message}", error.key),
            _ => error.key,
        })
    }

    /// Clear a previous gesture's error. Every producer calls this on entry: a
    /// new gesture supersedes the last one's outcome, so a success — or merely
    /// a retry in flight — never leaves the page accusing the user of a failure
    /// they have already moved past. A no-op (and **no** emit) when nothing is
    /// set, so the common path costs no observer tick.
    fn clear_page_error(&self) {
        if self.page_error.read().unwrap().is_none() {
            return;
        }
        *self.page_error.write().unwrap() = None;
        self.notify();
    }

    /// Confirm the add-participant overlay (`thread-add-participant-button` →
    /// confirm). Snapshot: a `(FaunaMls, OneToOne)` add forks a fresh
    /// participant-keyed group thread (Signal semantics) and selects it; every
    /// other `(rail, flavor)` adds in place. Wire op: an in-place add on an
    /// already-bound FaunaMls group posts the MLS Commit + Welcome via the
    /// backend; a 1:1 fork bootstraps its group lazily on first `send`
    /// (Track B), and non-FaunaMls rails have no wire membership op. Returns
    /// the resulting thread id, or `None` if the overlay wasn't open / no
    /// address was picked.
    pub async fn confirm_add_participant(&self) -> Option<ThreadId> {
        // Flush a typed-but-uncommitted recipient the way Enter would — probe,
        // then commit only what the probe confirmed (the same rule as
        // `send_new_thread`'s flush; a shape parse of the raw text is never a
        // chip — `accept_current_recipient_chip`'s doc). `resolve_recipient`
        // targets this overlay's picker while it is open.
        let has_pending_recipient = self
            .add_participant
            .read()
            .unwrap()
            .as_ref()
            .is_some_and(|s| s.picker.chips.is_empty() && !s.picker.raw_input.trim().is_empty());
        if has_pending_recipient {
            self.resolve_recipient().await;
            self.accept_current_recipient_chip();
        }
        // Take the overlay state out (overlay closes regardless of outcome).
        let state = self.add_participant.write().unwrap().take()?;
        let Some(addr) = state.picker.chips.first().cloned() else {
            self.notify();
            return None;
        };
        let target = state.target_thread_id.clone();
        let Some(detail) = self.threads.get(&target) else {
            self.notify();
            return None;
        };
        // Re-derived from the LIVE thread, never read off the overlay state:
        // `AddParticipantState::in_place_mls_group` is the paint's copy, for
        // the offline gate, and the wire op must not depend on a snapshot a
        // client held. The two agree by construction — one expression, and a
        // thread's rail/flavor are fixed at construction — which
        // `the_open_overlay_agrees_with_the_confirm_path` pins.
        let in_place_mls_group =
            crate::capabilities::is_in_place_mls_group(detail.rail, detail.flavor.clone());

        // Was this person already listed? `add_participant_to` is idempotent, so
        // this is what separates "we added them" from "they were already here" —
        // and therefore what the rollback below may and may not undo.
        let was_already_listed = detail
            .participants
            .iter()
            .any(|p| p.same_participant(&addr));

        // This gesture supersedes whatever the last one said.
        self.clear_page_error();

        // A community room's add is an INVITATION, and an invitation seats
        // nobody (`conversation-rooms.md` § Join rules and invites): the
        // invitee becomes a participant when they accept, which the room's
        // floor shows. So no optimistic chip — one that later vanished would
        // be the untruthful half-added member § M2 forbids — and no MLS
        // history slice, which is the end-to-end class's joiner history.
        let community = self
            .thread_detail(target.clone())
            .and_then(|d| d.room)
            .is_some_and(|room| room.class == crate::room::RoomClass::Community);

        // Snapshot mutation (fork for FaunaMls 1:1, else in-place).
        let result = if community {
            Some(target.clone())
        } else {
            self.add_participant_inner(target.clone(), addr.clone())
        };

        // Wire op for an in-place add on a (bound) FaunaMls group only.
        if in_place_mls_group {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend
                && let Err(e) = backend.add_participant(target.clone(), addr.clone()).await
            {
                // **Roll the optimistic add back.** The snapshot mutation runs
                // before the wire op, so leaving it in place on failure shows the
                // person as a participant of a group they are not in — the exact
                // inverse of the "rendered truthfully as not-yet-shared" property
                // `mls-group-key-material.md` § M2 requires of a half-added
                // member. Only undo what *we* added: a duplicate gesture on an
                // existing participant must not evict them from the list.
                if !was_already_listed {
                    self.threads.remove_participant_from(&target, &addr);
                }
                // …and **say so**. The rollback alone makes the failure honest;
                // without this the overlay closes, the list is unchanged and
                // nothing surfaces — indistinguishable from a dropped command.
                // `user_detail`, never `Display` — the page error and the send
                // slot are the same `error-message` element, so the send-slot
                // taxonomy governs both producers (conversations.md § Errors &
                // edge cases). The raw error goes to the log here.
                tracing::warn!("add participant failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_add_participant",
                    "message",
                    e.user_detail(),
                ));
            }
        }

        if let Some(new_id) = result.clone()
            && new_id != target
        {
            *self.selected.write().unwrap() = Some(new_id);
        }
        self.notify();
        // Rule 3: the membership edit rides the thread's history slice (the
        // participant list is snapshot state) — durable before done. The 1:1
        // fork's new thread has no channel yet (no-op); its first send
        // bootstraps + persists.
        if let Some(id) = &result {
            self.persist_thread_history(id).await;
        }
        // History for the joiner (`conversation-rooms.md` § History for
        // joiners): under the room's `full` rule the inviting device — this
        // one — re-seals its slice of the transcript to the newcomer, right
        // after the add so it lands in the newcomer's first epoch. Read off
        // the projected room, never re-derived: the rail refuses a slice the
        // policy does not authorize, this only decides whether to offer one.
        if in_place_mls_group
            && !community
            && self.page_error_diagnostic().is_none()
            && let Some(detail) = self.thread_detail(target.clone())
            && detail
                .room
                .as_ref()
                .and_then(|r| r.policy.as_ref())
                .is_some_and(|p| p.history_policy == crate::room::HistoryPolicy::Full)
            && let Some(channel_hex) = self.channel_hex(&target)
            && let Some(slice) = self.snapshot_channel_slice(&target, &channel_hex, 0)
        {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend
                && let Err(e) = backend.deliver_history_slice(target.clone(), &slice).await
            {
                // The member is in; only their view of the past is short.
                // Logged, never surfaced as the add's failure.
                tracing::warn!("history slice for the newcomer was not delivered: {e:?}");
            }
        }
        result
    }

    /// Change a governed room's join rule (`conversation-rooms.md` § Join
    /// rules and invites) — the policy editor's first field. Owner or admin;
    /// a refusal surfaces on the page's `error-message` like every other
    /// membership gesture.
    pub async fn set_room_join_rule(&self, id: ThreadId, rule: crate::room::JoinRule) {
        self.edit_room_policy(id, RoomPolicyEdit::JoinRule(rule))
            .await;
    }

    /// Change a governed room's history policy (`conversation-rooms.md`
    /// § History for joiners) — the policy editor's second field. Owner or
    /// admin.
    pub async fn set_room_history_policy(&self, id: ThreadId, policy: crate::room::HistoryPolicy) {
        self.edit_room_policy(id, RoomPolicyEdit::HistoryPolicy(policy))
            .await;
    }

    /// Appoint a member of a governed room an admin (owner only,
    /// `conversation-rooms.md` § Roles and authorization).
    pub async fn appoint_admin(&self, id: ThreadId, addr: TypedAddress) {
        match addr.person_actor_id() {
            Some(actor) => {
                self.edit_room_policy(id, RoomPolicyEdit::AppointAdmin(actor))
                    .await;
            }
            None => self.set_room_policy_error(BackendError::NotSupported.user_detail()),
        }
    }

    /// Demote an admin of a governed room to member (owner only).
    pub async fn demote_admin(&self, id: ThreadId, addr: TypedAddress) {
        match addr.person_actor_id() {
            Some(actor) => {
                self.edit_room_policy(id, RoomPolicyEdit::DemoteAdmin(actor))
                    .await;
            }
            None => self.set_room_policy_error(BackendError::NotSupported.user_detail()),
        }
    }

    /// Hand a governed room to another member (owner only,
    /// `conversation-rooms.md` § Roles and authorization → *Ownership
    /// transfer*). The owner's act posts the countersigned offer; the roles
    /// flip on every seat once the new owner's device has committed it, so
    /// the projection follows the agreed group context — not this call.
    pub async fn transfer_room_ownership(&self, id: ThreadId, addr: TypedAddress) {
        match addr.person_actor_id() {
            Some(actor) => {
                self.edit_room_policy(id, RoomPolicyEdit::TransferOwnership(actor))
                    .await;
            }
            None => self.set_room_policy_error(BackendError::NotSupported.user_detail()),
        }
    }

    /// `room-invitation-accept-button[i]` — accept the standing invitation
    /// `id` names: the account is seated on the room's floor, and the room
    /// opens as a thread and is selected. It reads nothing until a member with
    /// key authority keys it in, which happens on that member's device
    /// (`FaunaMlsBackend::tend_community_room`); the thread fills from then.
    ///
    /// Returns the room's thread, or `None` when the invitation is no longer
    /// standing or the accept was refused — the refusal on the page's
    /// `error-message`.
    pub async fn accept_room_invitation(&self, id: i64) -> Option<ThreadId> {
        self.clear_page_error();
        let invitation = self
            .room_invitations
            .read()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .cloned()?;
        let backend = self
            .backends
            .read()
            .unwrap()
            .get(&Rail::FaunaMls)
            .cloned()?;
        // Channel-keyed from the start, like every room this device joins
        // (`Self::materialize_conv_thread`): the room id IS its channel. The
        // inviter is the one member known before the floor is read; the next
        // floor read seats the rest.
        let thread = self.threads.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: invitation.room_id_hex(),
            },
            vec![self.seat_address_for(invitation.inviter)],
            ThreadFlavor::MlsGroup,
            None,
        );
        // The inviter's name was copied off another thread, if at all; its
        // provenance comes with it (`ThreadStore::inherit_anchor_grade`).
        self.threads.inherit_anchor_grade(&thread);
        match backend
            .accept_room_invitation(thread.clone(), &invitation)
            .await
        {
            Ok(()) => {
                self.room_invitations
                    .write()
                    .unwrap()
                    .retain(|i| i.id != id);
                *self.new_thread_active.write().unwrap() = false;
                *self.selected.write().unwrap() = Some(thread.clone());
                self.notify();
                self.persist_thread_history(&thread).await;
                Some(thread)
            }
            Err(e) => {
                // Nothing was bound, so the thread made for it never became a
                // room — drop it rather than leave an empty conversation behind.
                if backend.channel_binding_hex(&thread).is_none() {
                    self.threads.discard(&thread);
                }
                tracing::warn!("accepting a room invitation failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_room_invitation",
                    "message",
                    e.user_detail(),
                ));
                // A refusal may be the invitation LAPSING — the accept door
                // judges it again and consumes one its inviter could no
                // longer issue (`conversation-rooms.md` § Join rules and
                // invites). Re-list rather than guess from the error: what
                // stands is whatever the inbox still holds, and a row left on
                // screen would be a button that can only fail.
                self.refresh_room_invitations().await;
                None
            }
        }
    }

    /// `room-invitation-decline-button[i]` — the invitation stops standing for
    /// this account. The room is not told: a refusal the inviter could read
    /// would give declining an audience (`conversation-rooms.md` § Join rules
    /// and invites).
    pub async fn decline_room_invitation(&self, id: i64) {
        self.clear_page_error();
        let Some(invitation) = self
            .room_invitations
            .read()
            .unwrap()
            .iter()
            .find(|i| i.id == id)
            .cloned()
        else {
            return;
        };
        let Some(backend) = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned() else {
            return;
        };
        match backend.decline_room_invitation(&invitation).await {
            Ok(()) => {
                self.room_invitations
                    .write()
                    .unwrap()
                    .retain(|i| i.id != id);
                self.notify();
            }
            Err(e) => {
                tracing::warn!("declining a room invitation failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_room_invitation",
                    "message",
                    e.user_detail(),
                ));
            }
        }
    }

    /// Withdraw an invitation pending on the room `id` is — the gesture every
    /// row of [`crate::room::RoomSnapshot::pending_invites`] carries
    /// (`conversation-rooms.md` § Join rules and invites → *Pending invitations
    /// are visible to whoever may withdraw them*). `invitee_actor_hex` is the
    /// row's own `RoomPendingInviteSnapshot::invitee_actor_hex`.
    ///
    /// Acts at once, never staged through the editor's Save: a withdrawal is
    /// not a policy edit. No app gates it — the home nest served the row to
    /// this viewer *because* this viewer may withdraw it, and judges the act
    /// again at its own door. The invitee is told nothing. On success the list
    /// has been read again and the row is gone; a refusal lands on the page's
    /// `error-message`.
    pub async fn withdraw_room_invite(&self, id: ThreadId, invitee_actor_hex: String) {
        self.clear_page_error();
        let Ok(invitee) = ActorId::from_hex(&invitee_actor_hex) else {
            return;
        };
        let Some(backend) = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned() else {
            return;
        };
        match backend.withdraw_room_invite(id, invitee).await {
            Ok(()) => self.notify(),
            Err(e) => {
                tracing::warn!("withdrawing a room invitation failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_withdraw_room_invite",
                    "message",
                    e.user_detail(),
                ));
            }
        }
    }

    /// Grant or withdraw the home nest's read of the community room `id` is —
    /// the editor's `room-nest-read-toggle`, committed on Save through
    /// [`Self::apply_room_settings`]. A key rotation, and so the owner's or an
    /// admin's act; a refusal lands on the page's `error-message` like any
    /// other room-settings failure.
    pub async fn set_room_nest_read(&self, id: ThreadId, reads: bool) {
        self.clear_page_error();
        let Some(backend) = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned() else {
            return;
        };
        match backend.set_room_nest_read(id, reads).await {
            Ok(()) => self.notify(),
            Err(e) => {
                tracing::warn!("changing the home nest's read failed: {e:?}");
                self.set_room_policy_error(e.user_detail());
            }
        }
    }

    /// Replace the transparent labelers a community room's home nest applies
    /// to its messages — `labelers` are published labeler ids, lowercase hex
    /// (`conversation-rooms.md` § The three classes → *What the home nest does
    /// with its read*, purpose 2). Owner or admin; a refusal surfaces on the
    /// page's `error-message` like every other policy gesture.
    pub async fn set_room_labelers(&self, id: ThreadId, labelers: Vec<String>) {
        self.clear_page_error();
        let Some(backend) = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned() else {
            return;
        };
        match backend.set_room_labelers(id, labelers).await {
            Ok(()) => self.notify(),
            Err(e) => {
                tracing::warn!("naming the room's labelers failed: {e:?}");
                self.set_room_policy_error(e.user_detail());
            }
        }
    }

    /// `room-settings-save-button` — commit every change the editor staged,
    /// each as its own policy commit, in [`RoomSettingsDraft::edits`]' order
    /// (the two rules, then the appointments and demotions, then the
    /// hand-over last). **Stops at the first refusal**, which the calls above
    /// have already painted on the page's `error-message`, and returns
    /// whether all of them landed — the editor closes only on `true`
    /// (`ui/conversations.md` § Element IDs, the `room_settings` sub-page).
    ///
    /// The whole loop lives here rather than in each app's Save handler so
    /// the seven agree on the order and on what "landed" means (priority #2);
    /// an app stages through [`RoomSettingsDraft`] and calls this once.
    ///
    /// [`RoomSettingsDraft`]: crate::room_settings::RoomSettingsDraft
    /// [`RoomSettingsDraft::edits`]: crate::room_settings::RoomSettingsDraft::edits
    pub async fn apply_room_settings(
        &self,
        id: ThreadId,
        edits: Vec<crate::room_settings::RoomSettingsEdit>,
    ) -> bool {
        use crate::room_settings::RoomSettingsEdit;
        for edit in edits {
            match edit {
                RoomSettingsEdit::JoinRule { rule } => {
                    self.set_room_join_rule(id.clone(), rule).await;
                }
                RoomSettingsEdit::HistoryPolicy { policy } => {
                    self.set_room_history_policy(id.clone(), policy).await;
                }
                RoomSettingsEdit::Appoint { address } => {
                    self.appoint_admin(id.clone(), address).await;
                }
                RoomSettingsEdit::Demote { address } => {
                    self.demote_admin(id.clone(), address).await;
                }
                RoomSettingsEdit::NestRead { reads } => {
                    self.set_room_nest_read(id.clone(), reads).await;
                }
                RoomSettingsEdit::Labelers { labelers } => {
                    self.set_room_labelers(id.clone(), labelers).await;
                }
                RoomSettingsEdit::TransferOwnership { address } => {
                    self.transfer_room_ownership(id.clone(), address).await;
                }
            }
            if self.page_error_diagnostic().is_some() {
                return false;
            }
        }
        true
    }

    /// Rename a thread (`thread-rename-button` → `manager.rename_thread`).
    /// Snapshot: relabel immediately. Wire op: a bound FaunaMls group posts an
    /// encrypted `GroupMeta::NameChanged` application message so peers apply the
    /// rename via `poll_inbound_conv` → [`Self::apply_inbound_rename`].
    pub async fn rename_thread(&self, id: ThreadId, new_label: String) {
        self.clear_page_error();
        self.threads.rename(&id, new_label.clone());
        self.notify();
        let is_fauna_mls = self.threads.get(&id).map(|d| d.rail) == Some(Rail::FaunaMls);
        if is_fauna_mls {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend
                && let Err(e) = backend.rename(id.clone(), new_label).await
            {
                tracing::warn!("rename thread failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_rename_thread",
                    "message",
                    e.user_detail(),
                ));
            }
        }
        // Rule 3: the label rides the history slice — durable before done.
        self.persist_thread_history(&id).await;
    }

    /// **Leave the room `id` is** — the roles table's *leave (remove self)*
    /// row, and the gesture the departing-member report the nest has always
    /// admitted was missing a producer for (`conversation-rooms.md`
    /// § Roles and authorization → *Leaving — the mechanism*).
    ///
    /// **One verb, one door.** Every class leaves by the self-scoped
    /// `room.leave`, which stamps this account's floor row and no one else's.
    /// No app branches on class for this, and none should: the choice is the
    /// mechanism's, not the gesture's.
    ///
    /// **The owner is refused here, before the wire.** Both nest doors enforce
    /// "a room is never owner-less" on their own (one by rank, the other by
    /// refusing an owner-less roster for a floor that names an owner), so this
    /// check buys no safety — it buys the user a sentence that says what to do
    /// instead of a wire refusal that says a room was malformed. The paint is gated on
    /// `capabilities.can_leave_room` for the same reason, one layer up.
    ///
    /// **The thread stays.** This device keeps the generations and bubbles it
    /// already holds: deleting them would destroy the user's own copy of a
    /// conversation the user was legitimately part of, and would not un-read a
    /// byte.
    ///
    /// ⚠ **The leaver's own room verbs do NOT close yet** — declared, not
    /// overlooked (`conversation-rooms.md` § Implementation status today, the
    /// departed-render gap). `my_role` is read from the room's signed policy,
    /// which still names the leaver until a remaining owner or admin re-signs
    /// it, so this device keeps rendering the rank it held. Closing it wants a
    /// *departed* reading of the floor — this device has read the room's floor
    /// and it does not name this account — which also covers being removed by
    /// someone else, and is its own slice.
    pub async fn leave_room(&self, id: ThreadId) {
        self.clear_page_error();
        // `thread_detail`, never the raw store: the roles table becomes gating
        // only there (`RoomSnapshot::gate`), so the store's capabilities are
        // the rail's ungated answer and would let the owner walk out.
        let Some(detail) = self.thread_detail(id.clone()) else {
            return;
        };
        if !detail.capabilities.can_leave_room {
            // The owner's case is the one a user can actually reach with the
            // affordance greyed, via a stale snapshot: name the remedy.
            let owner = detail
                .room
                .as_ref()
                .and_then(|room| room.my_role)
                .is_some_and(|role| role == crate::room::RoomRole::Owner);
            let detail_text = if owner {
                fauna_i18n::strings::error::send::ROOM_OWNER_CANNOT_LEAVE
            } else {
                fauna_i18n::strings::error::send::NOT_SUPPORTED
            };
            self.set_page_error(LocalizedText::key_arg(
                "conversations.unified.error_leave_room",
                "message",
                detail_text.to_string(),
            ));
            return;
        }
        let backend = self.backends.read().unwrap().get(&detail.rail).cloned();
        let Some(backend) = backend else {
            return;
        };
        match backend.leave_room(&detail).await {
            Ok(()) => {
                // Nothing local to mutate: `participants` lists the OTHER
                // members, and the thread itself stays (see above). The floor
                // this device re-reads on the next poll is what carries the
                // departure into the render.
                self.notify();
            }
            Err(e) => {
                tracing::warn!("leaving a room failed: {e:?}");
                self.set_page_error(LocalizedText::key_arg(
                    "conversations.unified.error_leave_room",
                    "message",
                    e.user_detail(),
                ));
            }
        }
    }

    /// Remove a participant (`thread-member-chip[i]` →
    /// `manager.remove_participant`). Snapshot: drop the participant. Wire op:
    /// a bound FaunaMls group posts an MLS Commit that re-keys the group so the
    /// removed member can't follow forward (no Welcome). Non-FaunaMls rails are
    /// snapshot-only.
    pub async fn remove_participant(&self, id: ThreadId, addr: TypedAddress) {
        self.clear_page_error();
        if let Err(detail) = self.remove_participant_inner(id, addr).await {
            self.set_page_error(LocalizedText::key_arg(
                "conversations.unified.error_remove_participant",
                "message",
                detail,
            ));
        }
    }

    /// Remove `person` from **every** group of the owner's they are currently
    /// in — the *Remove* of the two review surfaces that render a person
    /// outside any one group (`identity-succession.md` § Propagation → *MLS
    /// groups*). See [`crate::eviction`] for why the operation lives here, and
    /// [`CrossGroupEviction::earned_verdict`] for what its outcome may and may
    /// not be recorded as.
    ///
    /// **Which groups: the ones the person is in *now*, re-derived — and
    /// re-derived from the authority, not from the chat snapshot.** The review
    /// item stores `(person, raising event, reason)` and deliberately not the
    /// groups, so this enumerates live membership. That is right for *removing*
    /// and would be wrong for *raising* — a raise is a fact about membership
    /// across the compromise window, and re-deriving it would flag people who
    /// joined afterwards (§ Propagation). Two operations, two different
    /// questions, one of which must never borrow the other's answer. *Which*
    /// live membership is the second half of the same rule, and it is the
    /// security-bearing half — see the contract note below.
    ///
    /// **A 1:1's honest peer is left alone**, and the gate is the thread
    /// *flavor* deliberately rather than the `supports_membership_change`
    /// capability the obvious draft reaches for. That capability cannot
    /// discriminate here: a `TypedAddress::Fauna` participant only ever appears
    /// on a FaunaMls thread, and **every** FaunaMls flavor is
    /// membership-capable, so the check is unreachable beside the actor-id
    /// match below — a guard that can never fire, and therefore one no test can
    /// ever pin (found by mutating it away and watching all three pins stay
    /// green). The reachable distinction is the one that matters anyway: a DM
    /// is not a group, and "removing" the only other person from a 1:1 would
    /// leave the owner alone in a thread the ordinary delete already handles —
    /// a second removal mechanism of exactly the kind § Propagation refuses to
    /// mint. What the flavor does **not** protect is a seat the 1:1 never
    /// promised — see rule (5) below.
    /// **⚠ Contract for every caller and every future edit: this driver's roster
    /// source must be the same source the flag it acts on was raised from.**
    /// That flag is `SweepReport::unattested_members`, read off the MLS **engine**
    /// (`MlsEngine::group_members`), so membership here is asked of the rail
    /// ([`crate::backend::RailBackend::authoritative_roster`]) and NOT of
    /// `ThreadDetail::participants`. The snapshot is a local view that no inbound
    /// Commit reconciles: where the two disagree — a foreign-authored add being
    /// the adversarial case — a snapshot-sourced enumeration `continue`s past the
    /// group, records no failure, and so earns `Removed` while the person is
    /// still seated, which is rule (3)'s harm reached through the enumeration
    /// door. A surface built on this must
    /// likewise never render *who remains* from the snapshot.
    ///
    /// **The span — § Propagation rule (5), ratified 2026-08-10.** The raise's
    /// span is every MLS group the engine holds; *Remove*'s reach is decided
    /// per channel class, and a raised seat it cannot clear rides
    /// [`CrossGroupEviction::unreachable`] as a typed fact — blocking the
    /// verdict, never escaping silently. Per class: a chat-**group** thread is
    /// evicted directly; a 1:1's one honest peer (the snapshot-listed
    /// participant) is never touched (rule (2)) but **any engine seat beyond
    /// the snapshot-listed peer is evicted like a group seat** — the chat poll
    /// applies membership Commits with no flavor gate, so a thief's Commit can
    /// seat a third identity in a DM channel; a chat channel with no bound
    /// thread here blocks as
    /// [`crate::eviction::UnreachableSeatClass::ChatGroupNoThreadHere`]; a
    /// folder channel blocks as
    /// [`crate::eviction::UnreachableSeatClass::FolderChannel`]
    /// (its removal is the folder plane's, and a left set converges the same
    /// way); a scheduling channel never blocks — no Commit is ever applied to
    /// one, so nothing can be planted there and nothing needs removing. The
    /// unbound enumeration follows the same source rule as the roster:
    /// [`crate::backend::RailBackend::unbound_seats_of`], off the engine.
    pub async fn evict_person_everywhere(&self, person: &ActorId) -> CrossGroupEviction {
        use crate::backend::UnboundChannelClass;
        use crate::eviction::{UnreachableSeat, UnreachableSeatClass};
        self.clear_page_error();
        let mut outcome = CrossGroupEviction::default();
        // Cloned out of the lock before the loop: the guard is a std lock and
        // cannot be held across the awaits below.
        let rail = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
        for summary in self.threads.list_summaries() {
            let Some(detail) = self.threads.get(&summary.thread_id) else {
                continue;
            };
            let listed = detail
                .participants
                .iter()
                .find(|p| crate::eviction::is_person(p, person))
                .cloned();
            // The authority answers when it has one; `None` means this rail keeps
            // no roster of its own for the thread (never bootstrapped, or a
            // non-MLS rail), and there the snapshot *is* the roster.
            let authority = rail
                .as_ref()
                .and_then(|b| b.authoritative_roster(&summary.thread_id));
            let (seated, is_extra_seat) = match (&detail.flavor, &authority) {
                // Rule (2): a 1:1 is not a group and its honest peer — the
                // participant the snapshot lists — is never Remove's business.
                // An engine seat the snapshot never listed is not that peer:
                // it is a planted third seat, evicted like any group seat.
                // Without an authoritative roster the snapshot is the whole
                // truth of a 1:1, and its only listee is the honest peer.
                (ThreadFlavor::OneToOne, Some(roster)) => {
                    (roster.contains(person) && listed.is_none(), true)
                }
                (ThreadFlavor::OneToOne, None) => (false, false),
                (ThreadFlavor::MlsGroup, Some(roster)) => (roster.contains(person), false),
                (ThreadFlavor::MlsGroup, None) => (listed.is_some(), false),
                // Non-MLS flavors (a subject-keyed mail thread) seat no
                // Fauna-addressed participant and hold no engine group. A
                // flavor this build does not name seats no one either: an
                // unknown arm never evicts.
                (ThreadFlavor::SubjectKeyed | ThreadFlavor::Unknown { .. }, _) => (false, false),
            };
            if !seated {
                continue;
            }
            // The snapshot's own entry when it has one, so an ordinary eviction
            // keeps its handle and `remove_participant_inner`'s rollback restores
            // the row the user was looking at. Otherwise the authority is all we
            // have: an actor id and no handle, which is exactly what the wire op
            // keys on (`FaunaMlsBackend::remove_participant` → `fauna_actor`).
            // (An extra 1:1 seat has no snapshot entry by construction.)
            debug_assert!(!is_extra_seat || listed.is_none());
            let addr = listed.unwrap_or(TypedAddress::Fauna {
                handle: String::new(),
                actor_id: *person,
            });
            // The roles table (`conversation-rooms.md` § Roles and
            // authorization), read off the projected room BEFORE the gesture:
            // on a governed room in which this viewer may not remove — a plain
            // member, or a target who is the owner — the seat is a typed
            // `unreachable` fact, never a retryable failure and never a silent
            // skip. The rail would refuse the same gesture with the same
            // sentence; classifying it here keeps the review surface's
            // verdict honest (the seat stands, `Removed` is not earned) and
            // names the remedy: the room's owner or an admin.
            let permitted = self
                .thread_detail(summary.thread_id.clone())
                .is_some_and(|d| {
                    d.capabilities.can_remove_members
                        && d.room.as_ref().is_none_or(|room| {
                            room.members
                                .iter()
                                .zip(d.participants.iter())
                                .find(|(_, p)| crate::eviction::is_person(p, person))
                                .is_none_or(|(m, _)| m.role != Some(crate::room::RoomRole::Owner))
                        })
                });
            if !permitted {
                let channel_hex = rail
                    .as_ref()
                    .and_then(|b| b.channel_binding_hex(&summary.thread_id))
                    .unwrap_or_default();
                outcome.unreachable.push(UnreachableSeat {
                    channel_hex,
                    class: UnreachableSeatClass::NotPermittedByRoomPolicy,
                });
                continue;
            }
            match self
                .remove_participant_inner(summary.thread_id.clone(), addr)
                .await
            {
                Ok(()) => outcome.evicted.push(summary.thread_id),
                Err(detail) => outcome.failed.push(crate::eviction::EvictionFailure {
                    thread: summary.thread_id,
                    reason: detail,
                }),
            }
        }
        // The thread-less remainder — the engine seats no thread can see. The
        // rail reports the facts; rule (5)'s dispatch is here, where a test can
        // red each arm on its own.
        if let Some(rail) = rail {
            for seat in rail.unbound_seats_of(person) {
                let class = match seat.class {
                    UnboundChannelClass::Chat => UnreachableSeatClass::ChatGroupNoThreadHere,
                    UnboundChannelClass::Folder => UnreachableSeatClass::FolderChannel,
                    // No Commit is ever applied to a scheduling channel, so a
                    // seat there cannot have been planted and cannot be
                    // removed — it neither blocks nor renders (§ Propagation
                    // rule (5)).
                    UnboundChannelClass::Scheduling => continue,
                };
                outcome.unreachable.push(UnreachableSeat {
                    channel_hex: seat.channel_hex,
                    class,
                });
            }
        }
        outcome
    }

    /// The handle `person` is currently seated under in the owner's own
    /// threads, for the two review surfaces that render a person **outside any
    /// one group** and therefore have no chip to read a name off.
    ///
    /// ⚠ **Display only, and never an identity claim.** The whole review exists
    /// because an identity a thief seated in a group may wear any handle it
    /// likes, so this is what to *show* the owner beside the row, never what to
    /// match on — every membership decision here keys on the actor id
    /// ([`crate::eviction::is_person`]).
    ///
    /// **`None` is an ordinary answer, not a failure**, and the permanent view
    /// is where it happens: that surface holds a backlog someone postponed, and
    /// a flagged person may have left every group since. A caller renders the
    /// row regardless — dropping it would hide an item nobody could then close,
    /// which is the failure this surface exists to prevent.
    ///
    /// Re-derived rather than stored, for [`Self::evict_person_everywhere`]'s
    /// reason: the review item deliberately keeps no group list, and a handle
    /// cached at raise time would go stale exactly when it matters (a rename
    /// between the sweep and the review).
    /// ⚠ **An empty display is `None`, not `Some("")`, and the skip happens
    /// DURING the search** (2026-08-10). A roster built from a Welcome carries
    /// no handles at all — `ingest_welcome` writes `handle: String::new()` for
    /// every member, because a leaf credential has none to give — so the
    /// seated-but-nameless row is the *ordinary* case for any group the owner
    /// joined rather than created, not a corner. Returning `Some("")` walked
    /// straight past every caller's no-name fallback (tui's
    /// `REVIEW_UNKNOWN_PERSON`) and rendered a review row with a blank where the
    /// person goes; the `None` arm was already written and already handled, and
    /// this is what makes it reachable. Filtering *inside* the scan rather than
    /// on its result is the load-bearing half: one person is commonly seated in
    /// several threads, and a nameless Welcome row would otherwise end the
    /// search and shadow a handle-bearing row one thread over.
    ///
    /// **Only a proven seat lends its handle** (`contacts.md` § The private
    /// overlay → *The paint gate*): a row is
    /// consulted only in a thread whose bound group holds `person` as a
    /// verified leaf ([`Self::proven_roster`]). A participant row's `(handle,
    /// actor id)` pair comes from a resolve answer the dial rule leaves unbound
    /// on the actor-id side, so a nest at the dialed domain that answers with
    /// this person's id would otherwise put ITS handle on the person's
    /// genuinely nameless seat one thread over — the same borrowed-identity
    /// harm as the nickname, reached through the backfill. A thread the rail
    /// keeps no roster for (not yet bootstrapped) lends nothing; once the group
    /// seats the person's own key, the handle the dialed domain routes to that
    /// key is theirs to lend.
    pub fn handle_for_person(&self, person: &ActorId) -> Option<String> {
        self.threads.list_summaries().into_iter().find_map(|s| {
            let detail = self.threads.get(&s.thread_id)?;
            let proven = self.proven_roster(&s.thread_id)?;
            if !proven.contains(person) {
                return None;
            }
            detail
                .participants
                .iter()
                .filter(|p| crate::eviction::is_person(p, person))
                .find_map(|p| p.person_handle().map(str::to_string))
        })
    }

    /// The address to seat for a roster actor id — the **one** resolution path
    /// both roster seating sites use (`conversation-rooms.md` § Implementation
    /// status today: "resolving a seated actor id to a handle — one follow-on
    /// serving both this arm and the Welcome's").
    ///
    /// `backends::fauna_mls::ingest_welcome` (the members a Welcome brings) and
    /// [`Self::apply_inbound_roster`] (a member another device added) both learn
    /// their members from the MLS engine roster, which carries actor ids and
    /// nothing else. Each used to seat `handle: String::new()` inline — so a
    /// member arrived nameless even when this very device was already rendering
    /// that same person, by name, one thread over.
    ///
    /// This resolves against that device-local knowledge
    /// ([`Self::handle_for_person`]) and seats the handle when it finds one.
    /// Three properties it is chosen for:
    ///
    /// - **Non-blocking, no I/O.** A pure in-memory scan of threads this device
    ///   already holds, so it is safe on [`Self::apply_inbound_roster`]'s path
    ///   — which runs inside the inbound poll's channel lock — and safe under
    ///   the e2e state provider's no-blocking-I/O rule (e2e convention 11's
    ///   corollary).
    /// - **Honest when it finds nothing.** The seat keeps its empty handle and
    ///   [`TypedAddress::display`] renders the actor's short id, so an
    ///   unresolved member is a member with an elided name rather than a blank
    ///   row.
    /// - **Never an identity claim.** A handle read off another thread is what
    ///   to *show*; every membership decision still keys on the actor id
    ///   ([`TypedAddress::same_participant`]) — [`Self::handle_for_person`]
    ///   carries the full argument.
    ///
    /// An actor this device has never met resolves to nothing today. The
    /// remaining leg is a *network* id-keyed handle read: neither the floor
    /// roster's member records
    /// (`fauna_protocol::conversations::RoomRosterMemberWire`) nor
    /// `fauna.profile.get` carries a handle to serve it yet.
    //
    pub fn seat_address_for(&self, actor: ActorId) -> TypedAddress {
        TypedAddress::Fauna {
            actor_id: actor,
            handle: self.handle_for_person(&actor).unwrap_or_default(),
        }
    }

    // ── Recipient resolution (async backend probe) ─────────────────
    //
    // The async counterpart to the synchronous format-only
    // `set_*_recipient_input`: `resolve_recipient` actually probes the rail
    // backends (`docs/goal/ui/conversations.md` § User actions
    // `recipient-picker-input` → `resolve_recipient` then
    // `accept_recipient_chip`) — the FaunaMls probe is a WS-RPC call behind the
    // backend seam, but the result it stamps back is plain snapshot state.

    /// Resolve the active recipient picker's current raw input across the
    /// registered rail backends, then move its `resolve_state` Resolving →
    /// Resolved / NotFound / Error and stash the resolved [`TypedAddress`] in
    /// `RecipientPickerState::resolved`. A following
    /// [`Self::accept_current_recipient_chip`] commits that *resolved* address
    /// (e.g. a 64-hex actor id promoted to `Fauna` by the FaunaMls key-package
    /// probe), not the format-only parse. The add-participant overlay's picker
    /// takes priority over the new-thread picker, matching
    /// `accept_current_recipient_chip`. No-op if neither picker is open or the
    /// input is empty/whitespace.
    pub async fn resolve_recipient(&self) {
        self.resolve_picker(PickerTarget::Active).await;
    }

    /// Restore the draft set from bytes the client glue fetched from `__drafts`
    /// and unsealed with the owner's `BackupKey` — the load-on-launch /
    /// cross-device catch-up path. Refreshes observers so the composer reflects
    /// the restored drafts *before* the probe below runs. A corrupt or
    /// unreadable blob is logged and ignored (start from empty drafts) rather
    /// than failing the surface.
    ///
    /// The restore **fills**: it never clears a slot and never overwrites a
    /// draft the user is composing into, however late the fetch lands
    /// ([`DraftStore::restore_from_bytes`] owns the rule and the reasons).
    ///
    /// **Why this is `async` — a restore owes the recipient picker a probe.**
    /// `conversations.md` § Errors & edge cases → *The picker tells the truth*
    /// rule 1 ratifies that a non-empty recipient input is never `Idle`: typing
    /// stamps `Resolving` and only [`Self::resolve_recipient`] moves it on. But
    /// what *rests* is `raw_input` without the probe output — a running probe
    /// cannot survive a relaunch, and resting `Resolving` would paint a spinner
    /// nothing ever resolves (`store::drafts::persistable_picker` owns that
    /// reasoning). So a restored picker arrives holding an address at `Idle`,
    /// which was a **dead state**: nothing re-probes it, no chip can be
    /// committed from it (`federation.md` § Peer-auth model → *Discovery-failure
    /// semantics* — a chip is only ever a probed address), and the user cannot
    /// even re-arm it by re-typing the same address, because every app shell
    /// suppresses the echo of an unchanged field. Restoring the invariant in
    /// each of the seven shells would be seven copies of the rule (priorities
    /// #1/#2), so the restore issues the probe itself and every app gets it by
    /// calling the one method it already calls.
    ///
    /// This is the restore at the **current** [`Self::identity_epoch`]. A shell
    /// whose fetch can straddle an identity change on a manager it keeps across
    /// the switch calls [`Self::restore_drafts_at`] with the epoch it captured
    /// before the fetch instead.
    pub async fn restore_drafts(&self, bytes: Vec<u8>) {
        self.restore_drafts_at(self.identity_epoch(), bytes).await;
    }

    /// [`Self::restore_drafts`] for a fetch the shell started at `epoch` (read
    /// from [`Self::identity_epoch`] **before** the fetch). If
    /// [`Self::clear_for_identity_change`] has run since, the bytes belong to
    /// the outgoing account and the call does nothing: no fill, no notify, and
    /// no recipient probe, so the incoming account's drafts are not shadowed,
    /// its next autosave does not re-seal the outgoing account's text into its
    /// own plane, and its rails are never asked to resolve the outgoing
    /// account's recipient (`account-scoping.md` § The scoping taxonomy — the
    /// writers of account-scoped state are retired by the same drop). The
    /// refusal comes before the fill because the fill is what arms the probe.
    pub async fn restore_drafts_at(&self, epoch: u64, bytes: Vec<u8>) {
        if self.fill_restored_drafts(epoch, &bytes) {
            self.resolve_restored_recipient(epoch).await;
        }
    }

    /// Top up the local actor's one-time key-package pool on the nest to
    /// `target`. Thin FFI-facing wrapper over the FaunaMls rail backend's
    /// [`RailBackend::ensure_keypackages`](crate::backend::RailBackend::ensure_keypackages)
    /// (the backend owns the `MlsEngine` that mints packages — the manager stays
    /// free of MLS crypto per `conversations.md` § Architectural rules #2).
    /// Login-time replenish is session-owned
    /// ([`ConversationsSession::start_receive_loop`](crate::ConversationsSession::start_receive_loop)
    /// runs it **after** the MLS state-replica restore — a restore swaps the
    /// engine's provider storage, so a package minted before it would lose its
    /// private init key; `devices.md` § Cross-device MLS group-state sync);
    /// settings pages drive manual refresh through the same surface. Returns the
    /// number uploaded (`0` if at/above target, or if no FaunaMls backend is
    /// registered on this client). `target` of `0` is a count-only no-op.
    ///
    /// A mint writes fresh private init keys into the engine's provider storage
    /// only — this notifies the observers so the debounced replica autosave
    /// persists them (an unsaved mint leaves peers holding key packages whose
    /// init keys exist nowhere durable).
    pub async fn ensure_keypackages(&self, target: u64) -> Result<u64, BackendError> {
        let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
        match backend {
            Some(b) => {
                let minted = b.ensure_keypackages(target).await?;
                if minted > 0 {
                    self.notify();
                }
                Ok(minted)
            }
            None => Ok(0),
        }
    }

    /// Publish the actor's mandatory **last-resort** key package if absent. Thin
    /// FFI-facing wrapper over the FaunaMls rail backend's
    /// [`RailBackend::ensure_last_resort_keypackage`](crate::backend::RailBackend::ensure_last_resort_keypackage).
    /// Idempotent (the nest keeps a single last-resort row per actor), so every
    /// login calls it (session-owned, after the replica restore — see
    /// [`Self::ensure_keypackages`]) to stay `addressable` after the one-time
    /// pool drains (`docs/goal/architecture/federation.md` § Key packages).
    /// No-op if no FaunaMls backend is registered on this client. Notifies the
    /// observers on success — the backend mints a fresh package each call, and
    /// its private init key must reach the replica autosave like any other mint.
    pub async fn ensure_last_resort_keypackage(&self) -> Result<(), BackendError> {
        let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
        match backend {
            Some(b) => {
                b.ensure_last_resort_keypackage().await?;
                self.notify();
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Toggle a reaction on `message` in `thread` (`dm-reaction-*` /
    /// `dm-reaction-add` — conversations.md § Reactions & message delete).
    /// FaunaMls-only: no-op if the thread has no `supports_reactions` capability
    /// or the local actor is unknown. Optimistically pushes an Add or Remove
    /// event onto the manager-owned reaction log (re-emitting so clients see the
    /// update immediately), then fires the backend wire op best-effort — a wire
    /// failure is warned, not propagated; the optimistic state stands.
    pub async fn toggle_reaction(&self, thread: ThreadId, message: MessageId, emoji: String) {
        let Some(detail) = self.thread_detail(thread.clone()) else {
            return;
        };
        if !detail.capabilities.supports_reactions {
            return;
        }
        let Some(me) = self.me_actor() else {
            return;
        };
        // Resolve Add vs Remove against my current set for this emoji (reuse fold).
        //
        // The optimistic event carries this device's own stamp, the same clock
        // and unit the wire op is about to seal under
        // (`post_room_body`/`post_app_message` stamp it themselves, a hair
        // later). The two are ordered against each other by nothing else, and
        // they never need to be: they are the same op, so whichever ranks
        // higher the fold reaches the same answer. What the stamp DOES buy
        // here is that this gesture outranks every op that preceded it —
        // including one some other party re-appended after it
        // (`crate::reactions::fold_reactions`), which a log-order fold could
        // not promise even for the user's own latest gesture.
        let now_ms = i64::try_from(fauna_core::data::Timestamp::now_millis()).unwrap_or(i64::MAX);
        let op = {
            let mut logs = self.reactions.write().unwrap();
            let entry = logs.entry(message.clone()).or_default();
            let currently = fold_reactions(entry, me)
                .iter()
                .any(|g| g.emoji == emoji && g.reacted_by_me);
            let op = if currently {
                ReactionOp::Remove
            } else {
                ReactionOp::Add
            };
            entry.push(StampedReactionEvent::new(
                me,
                emoji.clone(),
                op.clone(),
                now_ms,
            ));
            op
        };
        self.notify();
        // Wire op (best-effort; optimistic state already applied).
        if let Some(seq) = message
            .0
            .rsplit(':')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
        {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend
                && let Err(e) = backend.send_reaction(&thread, seq, &emoji, op).await
            {
                tracing::warn!("send_reaction wire op failed: {e}");
            }
        }
        // Rule 3: an own reaction is an own application message — folded into
        // the history slice and MLS-opaque to its author off the log. Durable
        // before done.
        self.persist_thread_history(&thread).await;
    }

    /// Delete `message` in `thread` (`dm-message-delete-button` —
    /// conversations.md § Reactions & message delete). FaunaMls-only, sender-
    /// only: no-op if the thread lacks `supports_message_delete`, or if the
    /// message is not owned by the local user. Optimistically tombstones the
    /// message in the manager-owned deleted set (re-emitting immediately), then
    /// fires the backend wire op best-effort — a wire failure is warned, not
    /// propagated; the optimistic state stands. The client renders the deleted
    /// placeholder off `MessageSnapshot.deleted`; body/attachments are left
    /// intact in the snapshot (the CLIENT decides the render, not the store).
    pub async fn delete_message(&self, thread: ThreadId, message: MessageId) {
        let Some(detail) = self.thread_detail(thread.clone()) else {
            return;
        };
        if !detail.capabilities.supports_message_delete {
            return;
        }
        // Own, or the viewer governs the room: reject any other target
        // (defence-in-depth; every member's ingest re-checks).
        let Some(is_own) = detail
            .messages
            .iter()
            .find(|m| m.message_id == message && m.can_delete)
            .map(|m| m.is_own)
        else {
            return;
        };
        self.deleted.write().unwrap().insert(message.clone());
        self.notify();
        if let Some(seq) = message
            .0
            .rsplit(':')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
        {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend {
                // The viewer's own message, or another member's the viewer
                // governs: two acts, because a community room carries them on
                // different wire shapes ([`ConversationBackend::send_delete_any`]).
                let sent = if is_own {
                    backend.send_delete(&thread, seq).await
                } else {
                    backend.send_delete_any(&thread, seq).await
                };
                if let Err(e) = sent {
                    tracing::warn!("delete wire op failed: {e}");
                }
            }
        }
        // Rule 3: the tombstone rides the history slice — durable before done.
        self.persist_thread_history(&thread).await;
    }
}

// Private helpers for the async client surface above.
//
// **`uniffi::export` does NOT ignore non-`pub` methods — keeping a helper here
// is what makes it private, not the `fn` visibility.** This comment used to
// claim the opposite, and that false premise is exactly what let
// `remove_participant_inner` be written inside the exported block, where its
// `Result<_, String>` redded all three merge gates at once (2026-08-10): the
// macro takes the whole `impl`, so a `String` error type compiles fine and then
// panics `uniffi_bindgen` with `unknown throw type: Some(String)` at generation
// time — on every platform that generates bindings, not just one. The two
// private methods that DO sit in the exported block (`set_page_error`,
// `clear_page_error`) are not evidence to the contrary; they merely happen to
// use types uniffi can lower and throw nothing.
//
// So: a helper that returns `Result<_, String>`, takes a non-uniffi type, or
// simply has no business on the foreign API surface goes HERE. Errors that
// genuinely cross the boundary use a `uniffi::Error` enum (`FfiError`), never a
// bare `String`.
impl ConversationsManager {
    /// The fill half of [`Self::restore_drafts_at`]: `false` when nothing was
    /// filled — `epoch` has moved (the bytes are the outgoing account's), or the
    /// blob would not open. The epoch read lock is held across the check and
    /// the fill, so an identity change (which holds it for writing across its
    /// bump and wipe) lands wholly before or wholly after; the notify runs
    /// after it is released.
    fn fill_restored_drafts(&self, epoch: u64, bytes: &[u8]) -> bool {
        {
            let current = self
                .identity_epoch
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if *current != epoch {
                tracing::debug!(
                    "drafts restore refused: started at identity epoch {epoch}, now {}",
                    *current
                );
                return false;
            }
            if let Err(e) = self.drafts.restore_from_bytes(bytes) {
                tracing::warn!("failed to restore drafts: {e}");
                return false;
            }
        }
        self.notify();
        true
    }

    /// Probe a recipient the restore just filled in, if it owes one: a
    /// non-empty `raw_input` resting at `Idle` on the new-thread picker. Both
    /// halves of the guard matter — an untouched picker is the one legitimate
    /// `Idle` (rule 1) and must not be stamped, and a picker already past `Idle`
    /// belongs to a live compose the restore declined to overwrite, whose own
    /// probe is either in flight or already answered.
    ///
    /// Skipped when `epoch` is no longer current: a switch landing while the
    /// fill notified wiped what the fill wrote, and the probe must not run on
    /// the incoming account's rails for it either way.
    async fn resolve_restored_recipient(&self, epoch: u64) {
        if self.identity_epoch() != epoch {
            return;
        }
        let owed = self
            .drafts
            .new_thread()
            .and_then(|c| c.recipient_picker)
            .is_some_and(|p| {
                p.resolve_state == ResolveState::Idle && !p.raw_input.trim().is_empty()
            });
        if owed {
            self.resolve_picker(PickerTarget::NewThread).await;
        }
    }

    /// The probe body behind [`Self::resolve_recipient`], parameterised by which
    /// picker it speaks for. A *user gesture* means [`PickerTarget::Active`]; a
    /// *restore* means [`PickerTarget::NewThread`], because the `__drafts` blob
    /// only ever carries that one slot ([`Self::restore_drafts`]). Here, not in
    /// the exported block: `PickerTarget` is a Rust-only type.
    async fn resolve_picker(&self, target: PickerTarget) {
        let Some(raw) = self.picker_raw_input(target) else {
            return;
        };
        if raw.trim().is_empty() {
            return;
        }

        // Optimistic Resolving while the probe runs (skipped if the user has
        // already typed past the input we're about to resolve).
        self.mutate_picker(target, |p| {
            if p.raw_input == raw {
                p.resolve_state = ResolveState::Resolving;
                p.resolved = None;
            }
        });

        let (state, resolved) = match self.probe_address(&raw).await {
            ResolveResult::Resolved(addr) => (ResolveState::Resolved, Some(addr)),
            ResolveResult::Pending => (ResolveState::Resolving, None),
            ResolveResult::Error(_) => (ResolveState::Error, None),
            // No rail claimed it. Fall back to the format-only parse so a
            // recipient on a rail with no registered backend (or one the probe
            // can't confirm) still commits by shape; an unrecognized shape is a
            // genuine not-found.
            ResolveResult::NotFound => match try_parse_typed_address(&raw) {
                Some(addr) => (ResolveState::Resolved, Some(addr)),
                None => (ResolveState::NotFound, None),
            },
        };

        // Stamp the result back, but only if the user hasn't typed past it.
        self.mutate_picker(target, |p| {
            if p.raw_input == raw {
                p.resolve_state = state;
                p.resolved = resolved;
            }
        });
    }

    /// The one path every policy edit takes: the thread's rail applies the
    /// edit under the viewer's role and commits it; the room projection on
    /// the next emit shows the result, and a refusal lands on the page error
    /// as `conversations.unified.error_set_room_policy`. Here, not in the
    /// exported block: `RoomPolicyEdit` is a Rust-only type.
    async fn edit_room_policy(&self, id: ThreadId, edit: RoomPolicyEdit) {
        self.clear_page_error();
        let backend = {
            let Some(rail) = self.threads.get(&id).map(|d| d.rail) else {
                return;
            };
            self.backends.read().unwrap().get(&rail).cloned()
        };
        let Some(backend) = backend else {
            return;
        };
        match backend.update_room_policy(id.clone(), edit).await {
            Ok(()) => self.notify(),
            Err(e) => {
                tracing::warn!("room policy edit failed: {e:?}");
                self.set_room_policy_error(e.user_detail());
            }
        }
    }

    fn set_room_policy_error(&self, detail: String) {
        self.set_page_error(LocalizedText::key_arg(
            "conversations.unified.error_set_room_policy",
            "message",
            detail,
        ));
    }

    /// The removal itself, reporting what happened instead of painting it.
    ///
    /// Split out of [`Self::remove_participant`] so the cross-group driver
    /// ([`Self::evict_person_everywhere`]) can learn **which** groups let go —
    /// the page-error shape there deliberately collapses that to one banner,
    /// which is right for a single chip tap and useless to a caller whose whole
    /// job is deciding whether the eviction was complete. One implementation,
    /// two reporting styles; a second copy of the optimistic-mutation-plus-
    /// rollback dance is exactly the drift priority #1 forbids.
    ///
    /// Rust-only by construction: `Result<_, String>` is unrepresentable on the
    /// UniFFI boundary (see this block's header), and the reporting shape it
    /// exists for has no foreign caller — the exported [`Self::remove_participant`]
    /// is what an app calls.
    async fn remove_participant_inner(
        &self,
        id: ThreadId,
        addr: TypedAddress,
    ) -> Result<(), String> {
        // Was this person listed at all? The removal is idempotent in the
        // store, so this is what separates "we dropped them" from "they were
        // never here" — and therefore what the rollback below may undo, exactly
        // as `was_already_listed` gates the add's.
        // Identity, not handle: this decides whether the rollback puts a row
        // back, so keying it on `display()` would restore a same-handle
        // bystander's row on a failure that concerned someone else
        // ([`TypedAddress::same_participant`]).
        let was_listed = self
            .threads
            .get(&id)
            .is_some_and(|d| d.participants.iter().any(|p| p.same_participant(&addr)));
        self.threads.remove_participant_from(&id, &addr);
        self.notify();
        let is_fauna_mls = self.threads.get(&id).map(|d| d.rail) == Some(Rail::FaunaMls);
        if is_fauna_mls {
            let backend = self.backends.read().unwrap().get(&Rail::FaunaMls).cloned();
            if let Some(backend) = backend
                && let Err(e) = backend.remove_participant(id.clone(), addr.clone()).await
            {
                // Same optimistic-mutation-first shape as the add, so the same
                // two halves: put the member back (they are still in the group
                // — hiding them is the inverse of rendering membership
                // truthfully), then surface why.
                if was_listed {
                    self.threads.add_participant_to(&id, addr);
                }
                tracing::warn!("remove participant failed: {e:?}");
                self.notify();
                // Rule 3 still applies to the rollback: the store was mutated
                // twice and its current truth (they are still here) is what has
                // to be durable.
                self.persist_thread_history(&id).await;
                return Err(e.user_detail());
            }
        }
        // Rule 3: the membership edit rides the history slice — durable before
        // done.
        self.persist_thread_history(&id).await;
        Ok(())
    }

    /// The active recipient picker's raw input — the add-participant overlay's
    /// picker takes priority over the new-thread compose picker (same precedence
    /// as [`Self::accept_current_recipient_chip`]). `None` if neither is open.
    /// Rule 3 (durable-before-done, `devices.md` § Durability rules): flush
    /// `id`'s history slice to durable storage through its rail backend,
    /// **awaited** — so the own store mutation the caller just made (send
    /// append, reaction, delete, rename, membership edit) is durable before the
    /// action completes. A failure is warn-logged, never surfaced: the wire op
    /// (if any) already succeeded, so failing the action would prompt a
    /// duplicate resend, and the mutation's own `notify()` has already armed
    /// the debounced replica autosave as the retry.
    async fn persist_thread_history(&self, id: &ThreadId) {
        let backend = {
            let Some(rail) = self.threads.get(id).map(|d| d.rail) else {
                return;
            };
            self.backends.read().unwrap().get(&rail).cloned()
        };
        if let Some(backend) = backend
            && let Err(e) = backend.persist_history(id).await
        {
            tracing::warn!("durable history flush failed (autosave retries): {e}");
        }
    }

    fn picker_raw_input(&self, target: PickerTarget) -> Option<String> {
        if target == PickerTarget::Active
            && let Some(state) = self.add_participant.read().unwrap().as_ref()
        {
            return Some(state.picker.raw_input.clone());
        }
        self.drafts
            .new_thread()
            .and_then(|c| c.recipient_picker)
            .map(|p| p.raw_input)
    }

    /// Apply `f` to `target`'s recipient picker, persisting the change and
    /// notifying. No-op if that picker is not present. Internal helper for the
    /// async resolve flow.
    fn mutate_picker(&self, target: PickerTarget, f: impl FnOnce(&mut RecipientPickerState)) {
        if target == PickerTarget::Active {
            let mut guard = self.add_participant.write().unwrap();
            if let Some(state) = guard.as_mut() {
                f(&mut state.picker);
                drop(guard);
                self.notify();
                return;
            }
        }
        if let Some(mut compose) = self.drafts.new_thread() {
            let mut picker = compose.recipient_picker.unwrap_or_default();
            f(&mut picker);
            compose.recipient_picker = Some(picker);
            self.drafts.set_new_thread(Some(compose));
            self.notify();
        }
    }

    /// Probe `raw` across the registered rail backends, returning the first
    /// `Resolved`. FaunaMls is tried first so a Fauna actor wins over the Email
    /// fallback once handle→actor lookup lands; the remaining rails follow in
    /// any order (distinct address shapes don't collide).
    ///
    /// **A rail's `Error` is terminal.** It means "this address is mine and I
    /// could not confirm it right now" ([`ResolveResult`]'s doc), and that is
    /// the answer: the chain stops there, so a later rail's *syntactic* claim on
    /// the same string — the SMTP rail resolves any `user@host` by shape — can
    /// never re-read a Fauna peer that did not answer as a plain email address
    /// and send in the clear (`docs/goal/architecture/federation.md` § Peer-auth
    /// model → *Discovery-failure semantics*, 2026-08-29; before this the error
    /// was merely *remembered* and SMTP won). Before probing, every rail is shown
    /// the participants of every thread ([`RailBackend::observe_participants`])
    /// — the FaunaMls rail's *known Fauna domain* evidence.
    async fn probe_address(&self, raw: &str) -> ResolveResult {
        let ordered: Vec<Arc<dyn RailBackend>> = {
            // A fixed order, never the map's: the bridged rail goes LAST
            // because its resolve is the only one with an effect — it asks
            // the nest, which opens the room for an address a bridge's grammar
            // admits (`rooms.open`) — so a string a format-only rail claims
            // never reaches it.
            let map = self.backends.read().unwrap();
            [Rail::FaunaMls, Rail::Smtp, Rail::Bridged]
                .iter()
                .filter_map(|rail| map.get(rail).cloned())
                .collect()
        };
        let participants: Vec<TypedAddress> = self
            .threads
            .list_summaries()
            .iter()
            .filter_map(|t| self.threads.get(&t.thread_id))
            .flat_map(|d| d.participants)
            .collect();
        for backend in &ordered {
            backend.observe_participants(&participants);
        }
        for backend in ordered {
            match backend.resolve_address(raw).await {
                ResolveResult::Resolved(addr) => return ResolveResult::Resolved(addr),
                ResolveResult::Error(e) => return ResolveResult::Error(e),
                ResolveResult::Pending | ResolveResult::NotFound => {}
            }
        }
        ResolveResult::NotFound
    }

    fn set_send_state(&self, id: &ThreadId, state: SendState) {
        let mut s = self.drafts.get(id);
        s.send_state = state;
        self.drafts.set(id.clone(), s);
        self.notify();
    }

    /// The local user's actor id, for reaction self-attribution — sourced from
    /// the FaunaMls backend's self_address (reactions are FaunaMls-only).
    fn me_actor(&self) -> Option<ActorId> {
        match self
            .backends
            .read()
            .unwrap()
            .get(&Rail::FaunaMls)?
            .self_address()?
        {
            TypedAddress::Fauna { actor_id, .. } => Some(actor_id),
            _ => None,
        }
    }

    /// Read thread `id` — the one place a read happens, for every rail's
    /// carrier (`conversation-read-state.md` § How the carriers meet the
    /// in-memory set): empty its unread set; on a native thread raise the
    /// channel's read marker (§ The read-marker record → *Who writes it*); for
    /// its unread `INBOX` mail owe the nest one batched `\Seen` write naming
    /// exactly those UIDs (§ Mail: `\Seen` is the marker → *Writing it*).
    /// Whether anything was unread.
    fn read_thread(&self, id: &ThreadId) -> bool {
        let inbox_uids = self.threads.unread_inbox_uids(id);
        let changed = self.threads.mark_read(id);
        if !changed {
            return false;
        }
        // The seam queues the raise; nothing here waits.
        if let Some((channel_hex, through)) = self.threads.raise_read_position(id)
            && let Some(seam) = self.read_positions.read().unwrap().seam.clone()
        {
            seam.raise(&channel_hex, through);
        }
        if !inbox_uids.is_empty() {
            self.mail_read.lock().unwrap().queue(inbox_uids);
            let poke = self.mail_read_poke.read().unwrap().clone();
            if let Some(poke) = poke {
                poke();
            }
        }
        true
    }

    /// Install the run-the-owed-`\Seen`-writes-now poke. Plain Rust, not
    /// UniFFI — the native session wires it in-process.
    pub fn set_mail_read_poke(&self, poke: Arc<dyn Fn() + Send + Sync>) {
        *self.mail_read_poke.write().unwrap() = Some(poke);
    }

    /// Every `\Seen` write reads have owed the nest since the last take,
    /// ascending, for one batched `fauna.email.inbox.mark_seen`. A batch the
    /// nest did not take goes back through [`Self::requeue_mail_seen`].
    pub fn take_owed_mail_seen(&self) -> Vec<u32> {
        self.mail_read.lock().unwrap().take_pending()
    }

    /// Whether reads owe the nest `\Seen` writes not yet taken — how a host
    /// with no in-process poke (web) knows to run its mail pass now.
    pub fn has_owed_mail_seen(&self) -> bool {
        self.mail_read.lock().unwrap().has_pending()
    }

    pub fn requeue_mail_seen(&self, uids: Vec<u32>) {
        self.mail_read.lock().unwrap().requeue(uids);
    }

    /// The launch drain's first `INBOX` page names the flag-change baseline;
    /// later offers are ignored ([`MailReadSync::offer_baseline`]).
    pub fn offer_mail_flag_baseline(&self, highest_modseq: u64) {
        self.mail_read
            .lock()
            .unwrap()
            .offer_baseline(highest_modseq);
    }

    /// Where the next `fauna.email.inbox.flag_changes` resumes, or `None`
    /// before a baseline exists or once the nest proved not to serve it.
    pub fn mail_flag_cursor(&self) -> Option<(u64, u32)> {
        self.mail_read.lock().unwrap().cursor()
    }

    /// The source does not serve the mail read-state kinds (a non-`INBOX` source,
    /// the trait default).
    /// Reads stay in memory for the run and nothing syncs, silently
    /// (`mail-app-surface.md` § Read state → *Compatibility*).
    pub fn note_mail_read_sync_unsupported(&self) {
        let mut sync = self.mail_read.lock().unwrap();
        if !sync.is_unsupported() {
            tracing::info!("source does not serve mail read-state sync; reads stay on this device");
        }
        sync.mark_unsupported();
    }

    /// Apply one delivered `flag_changes` page: each message that gained
    /// `\Seen` leaves the unread set, each that lost it re-enters, and the
    /// cursor moves past the page. Observers hear of it once, when anything
    /// moved.
    pub fn apply_mail_flag_changes(&self, page: &crate::backend::MailFlagChangesPage) {
        let mut moved = false;
        for change in &page.changes {
            // A UID read here whose write has not reached the nest yet: the
            // delivered flag set predates the read, which stands.
            if !change.has_seen_flag && self.mail_read.lock().unwrap().is_pending(change.uid) {
                continue;
            }
            moved |= self
                .threads
                .apply_inbox_seen(change.uid, change.has_seen_flag);
        }
        self.mail_read
            .lock()
            .unwrap()
            .advance(crate::mail_read::next_flag_cursor(page));
        if moved {
            self.notify();
        }
    }

    pub(crate) fn notify(&self) {
        // The thread the user has open is read, and so is whatever arrives in
        // it while it stays open (`conversations.md` § State & data shape →
        // *When a thread is read*). Here, and only here: every selection write
        // and every ingest ends in a notify, so this one line sits ahead of
        // every snapshot an observer takes, and no selection site or ingest
        // path has to remember it. The new-thread composer covers the selected
        // thread without deselecting it — a covered thread is not being read.
        if !*self.new_thread_active.read().unwrap()
            && let Some(attended) = self.selected.read().unwrap().clone()
        {
            self.read_thread(&attended);
        }
        for o in self.observers.read().unwrap().iter() {
            o.on_changed();
        }
    }

    /// A room fact the snapshot derives from the backend changed with no store
    /// write to carry it — [`crate::room::RoomSnapshot::awaiting_key`], which
    /// the walk flips without ingesting anything, and
    /// [`crate::room::RoomSnapshot::pending_invites`], which the floor's tend
    /// re-lists. Repaints, nothing else.
    pub(crate) fn notify_room_state_changed(&self) {
        self.notify();
    }

    /// Strip privacy metadata, hash the result (BLAKE3), cache it under that
    /// lowercase-hex `blob_hash` in the attachment store, and return the light
    /// staged draft. Shared by the per-thread + new-thread add paths.
    /// `is_image` is derived from the MIME so the bubble can pick
    /// `dm-attachment-image` vs `dm-attachment-file`.
    ///
    /// **The strip lives here, not in each app's file-picker glue.** It was
    /// per-app until 2026-07-20, and half the apps that implemented the
    /// picker forgot it — windows + android stripped, linux + web shipped GPS
    /// coordinates on the day their legs landed. A privacy step every app
    /// must remember is the wrong shape; every attachment reaches the wire
    /// through this one function, so it is the seam that can actually hold the
    /// guarantee. `fauna_media::strip_metadata` is lossless (container
    /// segment/chunk removal, never a decode/re-encode — see its own docs),
    /// preserves C2PA (JUMBF/APP11 is kept), and passes non-image or
    /// unparseable bytes through byte-identical, so it is safe for the
    /// arbitrary files a conversation attachment may carry and idempotent for
    /// the two apps that still strip on their side.
    ///
    /// Hashing happens *after* the strip on purpose: `blob_hash` is both the
    /// render handle and the wire reference, so hashing the raw bytes would
    /// leave every receiver's content-address disagreeing with the payload.
    ///
    /// **`register` runs before the bytes enter the store.** It puts the draft
    /// on its compose, which is what pins it
    /// (`DraftStore::staged_attachment_hashes`). Registering after the insert
    /// left the just-staged bytes unpinned for as long as that took, so a
    /// receive-path insert on a store at budget could evict them; taking the
    /// registration as a closure makes the order this function's, not each
    /// caller's (`conversations.md` § Attachments → *Retention*). Returns the
    /// `blob_hash`.
    fn stage_attachment(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
        register: impl FnOnce(AttachmentDraft),
    ) -> String {
        let bytes = fauna_media::process::strip_metadata(&bytes);
        let blob_hash = blake3::hash(&bytes).to_hex().to_string();
        let size_bytes = bytes.len() as u64;
        let is_image = mime_type.starts_with("image/");
        register(AttachmentDraft {
            blob_hash: blob_hash.clone(),
            filename,
            mime_type,
            size_bytes,
            is_image,
        });
        // Routed through the verifying door ([`Self::cache_attachment_bytes_checked`])
        // rather than a bare `insert`, so the store's content-address guarantee has
        // exactly one writer, pinned, instead of resting on this call site
        // independently computing the hash it inserts under — true today, but
        // unenforced (a security review finding). The re-hash costs one BLAKE3
        // pass over already-stripped bytes; the debug_assert documents that it
        // cannot fail here, since `blob_hash` above came from hashing these same
        // `bytes`.
        let cached = self.cache_attachment_bytes_checked(&blob_hash, bytes);
        debug_assert!(
            cached,
            "stage_attachment hashed `bytes` itself to produce blob_hash; \
             cache_attachment_bytes_checked must accept its own hash"
        );
        blob_hash
    }

    /// Resolve a compose's light attachment drafts to [`ResolvedAttachment`]s
    /// (metadata + bytes pulled from the store), for the backend's `send`.
    /// **All or refuse:** a draft whose bytes the store does not hold — one
    /// restored after a relaunch, or synced from another device, where the bytes
    /// never were — refuses the whole send with a [`BackendError::Refusal`]
    /// naming the file, so the user attaches it again. It is never skipped: a
    /// skipped attachment is a message sent without the file the user attached
    /// (`conversations.md` § Persistence).
    fn resolve_attachments(
        &self,
        compose: &ComposeState,
    ) -> Result<Vec<ResolvedAttachment>, BackendError> {
        let store = self.attachments.read().unwrap();
        compose
            .attachments
            .iter()
            .map(|d| {
                let bytes = store.peek(&d.blob_hash).ok_or_else(|| {
                    BackendError::Refusal(fauna_i18n::strings::error::send::attachment_missing(
                        &d.filename,
                    ))
                })?;
                Ok(ResolvedAttachment {
                    blob_hash: d.blob_hash.clone(),
                    filename: d.filename.clone(),
                    mime_type: d.mime_type.clone(),
                    is_image: d.is_image,
                    bytes: bytes.to_vec(),
                })
            })
            .collect()
    }

    /// Map the attachments a send actually handed its backend onto the
    /// `AttachmentSnapshot`s for the sender's own "Sent" copy — built from the
    /// resolved set, never from the compose, so the sender's bubble cannot list
    /// a file the wire did not carry (`conversations.md` § Persistence). The
    /// bytes are in the store (`resolve_attachments` just read them there), so
    /// the bubble renders the real attachment too. `c2pa` is `false` here — the
    /// SMTP rail can't detect C2PA client-side (the heavy `fauna-media`
    /// pipeline is off on the FFI/WASM build).
    fn sent_echo_attachments(resolved: &[ResolvedAttachment]) -> Vec<AttachmentSnapshot> {
        resolved
            .iter()
            .map(|a| AttachmentSnapshot {
                blob_hash: a.blob_hash.clone(),
                filename: a.filename.clone(),
                mime_type: a.mime_type.clone(),
                size_bytes: a.bytes.len() as u64,
                is_image: a.is_image,
                c2pa: false,
            })
            .collect()
    }
}

/// The Fauna actor id of a message's sender, or None for a non-Fauna sender.
/// The picker state a raw-input write stamps before any probe has run: `Idle`
/// for empty input, else `Resolving` — the probe is owed, and only
/// `ConversationsManager::resolve_recipient` moves the state to a terminal
/// value. Never derived from the text's shape (`docs/goal/ui/conversations.md`
/// § Errors & edge cases → *The picker tells the truth*).
fn unprobed_resolve_state(text: &str) -> ResolveState {
    if text.trim().is_empty() {
        ResolveState::Idle
    } else {
        ResolveState::Resolving
    }
}

fn sender_actor(m: &MessageSnapshot) -> Option<ActorId> {
    match &m.sender {
        TypedAddress::Fauna { actor_id, .. } => Some(*actor_id),
        _ => None,
    }
}

/// The content-index kind a rail's messages are indexed as — mail as `Mail`,
/// every other rail (FaunaMls and the bridged DM rails) as `Conversation`.
/// One mapping for every site that offers a message (the inbound chokepoint,
/// the Sent-copy arm, the send), so a sent message and its received twin can
/// never be sealed under different class keys.
fn index_kind_for_rail(rail: Rail) -> IndexableKind {
    match rail {
        Rail::Smtp => IndexableKind::Mail,
        _ => IndexableKind::Conversation,
    }
}

fn infer_flavor(bucket: &crate::backend::InboundBucket) -> ThreadFlavor {
    if bucket
        .subject
        .as_deref()
        .map(crate::keying::normalize_subject)
        .filter(|s| !s.is_empty())
        .is_some()
    {
        ThreadFlavor::SubjectKeyed
    } else if bucket.participants.len() == 2 {
        ThreadFlavor::OneToOne
    } else {
        // Multi-party with no subject: treat as MlsGroup if rail is FaunaMls; else SubjectKeyed
        match bucket.rail {
            Rail::FaunaMls => ThreadFlavor::MlsGroup,
            _ => ThreadFlavor::SubjectKeyed,
        }
    }
}

/// Reorder the thread list per the active [`SortOrder`]. `list_summaries`
/// already returns latest-activity-first, but the sort is reapplied here so the
/// ordering is owned by one place and `OldestFirst` / `Unread` are honored.
/// `Unread` puts threads with unread messages first, latest-activity within
/// each group — so with nothing unread it reads exactly like latest-activity.
/// Filter the thread list by the active `conversation-search-box` query
/// (conversations.md § User actions — "Filter list"). A `None` or
/// blank/whitespace-only query is a no-op (an empty search box shows every
/// thread); otherwise a thread is kept iff the query is a case-insensitive
/// substring of its `label` (thread name) or `snippet` (latest-message
/// preview). Owning the filter here keeps every app a dumb renderer of the
/// already-filtered `snapshot().threads` (priority #3) and resolves the
/// per-app filter drift: windows hand-rolled a `.Where()` over label+snippet
/// while web/linux/android did not filter at all (priority #4).
/// The list filter: a case-insensitive substring over each thread's label and its
/// latest message's FULL plaintext — never the bounded `snippet` preview, so a term past
/// what a row shows still finds its thread (`conversations.md` § Where logic lives →
/// *Thread-list sort + search filtering*). The body match runs only while a query is set.
fn filter_summaries(threads: &mut Vec<ThreadSummary>, query: Option<&str>, store: &ThreadStore) {
    let needle = match query {
        Some(q) => q.trim().to_lowercase(),
        None => return,
    };
    if needle.is_empty() {
        return;
    }
    threads.retain(|t| {
        t.label.to_lowercase().contains(&needle)
            || store.latest_plaintext_matches(&t.thread_id, &needle)
    });
}

fn sort_summaries(threads: &mut [ThreadSummary], sort: SortOrder) {
    match sort {
        SortOrder::LatestActivity => {
            threads.sort_by_key(|s| std::cmp::Reverse(s.last_activity_ms));
        }
        SortOrder::OldestFirst => {
            threads.sort_by_key(|s| s.last_activity_ms);
        }
        SortOrder::Unread => {
            threads.sort_by_key(|s| (s.unread_count == 0, std::cmp::Reverse(s.last_activity_ms)));
        }
    }
}

/// A process-unique synthetic message id for a test-seam inject with no
/// explicit `message_id`. A monotonic counter (not a uuid dep) — unique
/// within a test run, which is all `in_reply_to` threading needs.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn next_test_message_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("msg-{}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

// The e2e injection seams. Two gates, deliberately different, per
// `docs/goal/architecture/testing.md` § Cross-app e2e conventions, convention 15
// (the automation surface is compiled out of release artifacts):
//
// * **Visibility** carries the `debug_assertions` arm so a plain debug build of an
//   in-process Rust consumer (linux, tui) can reach these without the dep having
//   to name the feature — the exact mirror of the apps' own agent gate,
//   `cfg(any(debug_assertions, feature = "e2e-agent"))`. `just linux-debug` /
//   `tui-debug` pass no features, so the harness relies on this arm. A release
//   build turns it off, which is the security property: the seams are absent from
//   the shipped binary unless a release-profile e2e build opts in via the feature.
// * **The `uniffi::export` cfg stays keyed on the FEATURE ONLY** — never on the
//   profile. That is load-bearing: it makes the generated Kotlin/Swift/C#/Go faces
//   a pure function of the feature set, so debug and release builds of the same
//   feature set cannot disagree about the FFI surface and no binding regen is owed
//   for a profile change. Do not "simplify" the two gates into one.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
impl ConversationsManager {
    /// Test-only inbound injection that bypasses needing a real backend
    /// connection. Foreign callers (FlaUI bridge for the Windows e2e)
    /// reach this through the same UniFFI surface as the regular methods
    /// when both `uniffi` and `test-helpers` are enabled.
    pub fn inject_inbound_for_test(&self, msg: RailInboundMessage) -> Result<(), BackendError> {
        self.ingest_inbound(msg)
    }

    /// Test-only: inject a message and force its `is_own` flag to `true`, so a
    /// client e2e can materialise an **own** bubble — and exercise the
    /// sender-only delete affordance + the own-reaction pill — without a live
    /// cross-member send. On clients that wire the REAL FaunaMls backend at login
    /// (e.g. linux) a compose-send can't produce the own echo: the backend has no
    /// peer keypackage on the nest for an injected/groupless thread, so the send
    /// fails. This keys the thread through the same `ingest_inbound` path as
    /// [`Self::inject_inbound_for_test`], then flips the just-added message's
    /// `is_own` (mirrors [`Self::inject_inbound_with_subject_change_for_test`]).
    pub fn inject_own_for_test(&self, msg: RailInboundMessage) -> Result<(), BackendError> {
        let msg_id = msg.message_id.clone();
        self.ingest_inbound(msg)?;
        self.threads.set_message_is_own_for_test(&msg_id, true);
        self.notify();
        Ok(())
    }

    /// Test-only: inject a message and stamp its content [`labels`] directly, so
    /// a client e2e can materialise a *labeled* inbound bubble — the family-safety
    /// content-floor render, the content-label badge — without the real MLS receive
    /// path. The generic inject path ([`Self::inject_inbound_for_test`] ->
    /// `ingest_inbound`) does not classify (only `ingest_inbound_to_thread`'s
    /// `observe_local_detection` does), so the labels are staged after append, the
    /// same shape as the feed's `TestPostSpec.labels`. Mirrors
    /// [`Self::inject_own_for_test`].
    ///
    /// [`labels`]: crate::message::MessageSnapshot::labels
    pub fn inject_inbound_with_labels_for_test(
        &self,
        msg: RailInboundMessage,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    ) -> Result<(), BackendError> {
        let msg_id = msg.message_id.clone();
        self.ingest_inbound(msg)?;
        self.threads.set_message_labels_for_test(&msg_id, labels);
        self.notify();
        Ok(())
    }

    /// Test-only inject path that lets the caller override the
    /// resulting message's <c>subject_line</c> regardless of the
    /// thread-keying subject. Used by the e2e test_subject_divider
    /// suite to drive divider rendering in a thread that was already
    /// keyed under a different subject — the bucket's <c>subject</c>
    /// keeps routing the message into the existing thread; the
    /// override then stamps the message's display subject line.
    pub fn inject_inbound_with_subject_change_for_test(
        &self,
        msg: RailInboundMessage,
        subject_change: String,
    ) -> Result<(), BackendError> {
        let msg_id = msg.message_id.clone();
        self.ingest_inbound(msg)?;
        self.threads
            .set_message_subject_line(&msg_id, subject_change);
        self.notify();
        Ok(())
    }

    /// Test-only: cache `bytes` under their content-addressed `blob_hash`
    /// (lowercase-hex BLAKE3) and return the matching [`AttachmentSnapshot`], so
    /// an `inject_inbound_for_test` message can carry a *renderable* attachment.
    /// The per-app bubble then resolves the handle back to these bytes via
    /// [`Self::attachment_bytes`] — exactly as a real inbound MIME parse would
    /// ([`crate::backends::smtp::ingest_inbound_record`] runs
    /// `extract_attachments` + [`Self::cache_attachment_bytes`]). `is_image` is
    /// derived from the MIME (the same rule as `stage_attachment`); `c2pa` is
    /// the receive path's own probe (`fauna_media::process::detect_c2pa`, as
    /// `attachments_to_inbound` runs it over a delivered attachment), so an
    /// injected signed picture carries the real per-attachment verdict — real on
    /// native builds, the stub `false` on wasm32 (`conversations.md`
    /// § Attachments "C2PA on-device"). Mirrors what `add_attachment`
    /// does for the compose side, so a cross-app e2e can assert
    /// `dm-attachment-image` / `dm-attachment-file` renders without a live
    /// backend round-trip.
    pub fn make_attachment_for_test(
        &self,
        filename: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> crate::message::AttachmentSnapshot {
        let blob_hash = blake3::hash(&bytes).to_hex().to_string();
        let size_bytes = bytes.len() as u64;
        let is_image = mime_type.starts_with("image/");
        let c2pa = fauna_media::process::detect_c2pa(&mime_type, &bytes);
        self.cache_attachment_bytes(blob_hash.clone(), bytes);
        crate::message::AttachmentSnapshot {
            blob_hash,
            filename,
            mime_type,
            size_bytes,
            is_image,
            c2pa,
        }
    }

    /// Test-only: drop the cached bytes of every attachment named `filename` in
    /// thread `id`, exactly as the store's budget eviction drops one entry — the
    /// bytes go, the remembered coordinates stay — and redraw, so the next render
    /// misses. A handle with coordinates is then fetched again by the next receive
    /// cycle; one with none (an injected attachment, a record the mailbox no
    /// longer holds) renders declared — filename and size — from then on
    /// (`conversations.md` § Attachments → *Retention*). Lets an e2e reach both
    /// without first filling the 128 MiB store. Returns how many handles were
    /// evicted, so a caller can refuse a no-op rather than assert on a render
    /// that never lost its bytes.
    pub fn evict_thread_attachments_for_test(&self, id: ThreadId, filename: String) -> u32 {
        let Some(detail) = self.thread_detail(id) else {
            return 0;
        };
        let hashes: Vec<String> = detail
            .messages
            .iter()
            .flat_map(|m| crate::message::attachment_blocks(&m.document))
            .filter(|a| a.filename == filename)
            .map(|a| a.blob_hash)
            .collect();
        let evicted = {
            let mut store = self.attachments.write().unwrap();
            hashes.iter().filter(|h| store.evict(h)).count() as u32
        };
        if evicted > 0 {
            self.notify();
        }
        evicted
    }

    /// Test-only: the shared inject-payload parser
    /// ([`Self::inject_inbound_from_test_payload`]) behind a JSON string — the
    /// face an app's `conversations_inject_inbound` handler hands the payload to
    /// instead of re-parsing it itself (tui and linux call the parser in
    /// process). One parser is one seam: `recipients`, the mail rail's real-self
    /// resolution, attachments, labels, `is_own` and `force_subject_change`
    /// behave the same on every app, and a key added to the payload reaches all
    /// of them at once. A payload that is not a JSON object is refused.
    pub fn inject_inbound_from_test_json(&self, payload_json: String) -> Result<(), BackendError> {
        let payload: serde_json::Value = serde_json::from_str(&payload_json)
            .map_err(|e| BackendError::Internal(format!("inject payload is not JSON: {e}")))?;
        let object = payload.as_object().ok_or_else(|| {
            BackendError::Internal("inject payload is not a JSON object".to_string())
        })?;
        self.inject_inbound_from_test_payload(object)
    }

    /// Test-only: stamp a **pre-resolved** [`PreviewState::Resolved`] for `url` into the
    /// link-preview cache — the conversations twin of the feed's `link_preview` inject spec
    /// (`tests/e2e-unified/tests/test_feed_link_preview.py`). A *real* resolve needs a live nest
    /// fetch of an OpenGraph page (SSRF-guarded, render-model.md § D4), so the cross-app e2e
    /// seeds the `Resolved` card deterministically instead of driving `fauna.linkpreview.resolve`.
    /// [`thread_detail`](Self::thread_detail) then folds this onto the matching bubble's
    /// `RenderBlock::LinkPreview` block (the preview fold runs BEFORE the D3 reveal walk), so the
    /// card paints title/description/domain immediately, and the og:image only after the message's
    /// `load-remote-content-button` reveal. `revealed: false` at seed, exactly as
    /// [`Self::resolve_link_preview`] stores it; `notify()` re-renders. An injected bubble whose
    /// body is a standalone `[url](url)` paragraph carries the `LinkPreview { Resolving }` block
    /// the fold needs (FaunaMls inbound is `BodyFormat::Markdown`), so seed + inject in either
    /// order: `notify` re-renders and the fold finds the now-`Resolved` URL.
    pub fn seed_resolved_link_preview_for_test(
        &self,
        url: String,
        title: String,
        description: String,
        image_hash: Option<String>,
    ) {
        self.resolved_previews.write().unwrap().insert(
            url,
            PreviewState::Resolved {
                title,
                description,
                image_hash,
                revealed: false,
            },
        );
        self.notify();
    }

    /// Test-only: drive a thread's compose into `Failed { reason }` and select
    /// it — the exact observable state `send`/`send_new_thread` leave after a
    /// backend send error (the rejecting-sink path in
    /// `failed_send_stamps_send_state_failed_for_surfacing`). The client's
    /// page-level `error-message` surface (`docs/goal/ui/conversations.md`
    /// § Errors & edge cases; linux `views/conversations/detail.rs` reads the
    /// active compose's `send_state`) then renders the reason. There is no
    /// *product* path that fails a send on demand — a mail-OFF nest enqueues
    /// the outbound row and returns `Ok` (`email_handlers::enqueue_outbound`),
    /// so the client send succeeds into a void and never reaches `Failed` — so
    /// the cross-app e2e drives this seam to assert the error surfaces.
    /// Mirrors `send_new_thread`, which selects the materialized thread before
    /// `send` stamps the failure onto its draft; `set_send_state` notifies
    /// observers, so the select + stamp land in one observer tick.
    ///
    /// Takes the backend detail as a plain `String` (not a `LocalizedText`)
    /// because the key is not a caller's choice for a send — [`SendState::failed`]
    /// owns it. That keeps the cross-app `conversations_inject_send_failure`
    /// command's flat `{thread_id, reason?}` payload — a contract implemented in
    /// every app's test agent (`testing.md` point 11) — identical to the detail a
    /// real backend rejection carries.
    pub fn inject_send_failure_for_test(&self, id: &ThreadId, reason: String) {
        *self.selected.write().unwrap() = Some(id.clone());
        self.set_send_state(id, SendState::failed(reason));
    }

    /// Test-only: drive [`ConversationsSnapshot::error`] — the exact observable
    /// state a failed membership/label wire op leaves. Exists for the same
    /// reason [`Self::inject_send_failure_for_test`] does: there is no *product*
    /// path that fails one of those ops on demand (a failure needs a nest that
    /// rejects the Commit, or a member whose roster the owner's nest cannot
    /// read), so the per-app `error-message` surface would otherwise be
    /// unpinnable from a client-side test.
    ///
    /// `notify`s, so the stamp reaches observers in one tick exactly as the real
    /// producers' [`Self::set_page_error`] does.
    pub fn inject_page_error_for_test(&self, error: LocalizedText) {
        self.set_page_error(error);
    }

    /// Test-only: force [`Self::receive_stopped`]'s `true` arm — the
    /// dead-receive-rail condition `ConversationsVM.pageError` ranks under
    /// [`Self::engine_served_elsewhere`] (`ui/conversations.md` § Errors &
    /// edge cases) — without a real panicked receive loop. Exists for the
    /// same reason [`Self::inject_page_error_for_test`] does: there is no
    /// *product* path that kills a receive loop on demand, so the seam
    /// drives the crate-internal generation dance
    /// ([`Self::begin_receive_loop`] + [`Self::mark_receive_stopped`])
    /// directly — claiming a fresh generation and immediately marking it
    /// stopped, exactly as a real supervised loop's first panic would leave
    /// it.
    pub fn mark_receive_stopped_for_test(&self) {
        let generation = self.begin_receive_loop();
        self.mark_receive_stopped(generation);
    }

    /// Test-only: drive [`Self::unopenable_mail_count`]'s floor arm without a
    /// real unopenable record — [`Self::note_unopenable_mail`] lives outside
    /// the FFI export block (crate-internal, `MailFeed` has no `uniffi`
    /// derive), so the per-app page-error test needs its own seam. Always
    /// `MailFeed::Inbox`, the only feed the floor notice covers
    /// (`mail-app-surface.md` § Inbound client receive → *Scope*).
    pub fn note_unopenable_mail_for_test(&self, uid: u32) {
        self.note_unopenable_mail(MailFeed::Inbox, uid);
    }

    /// Test-only state wipe: the harness's between-tests reset, so each test
    /// starts from a known empty state. Clears thread store, drafts, selection,
    /// search, and sort but preserves registered backends and observers.
    ///
    /// A thin alias for the production
    /// [`Self::clear_for_identity_change`] — same wipe, different caller. Kept
    /// as its own gated name because the e2e command tables across all 7 apps
    /// (`testing.md` point 11) speak it, and because a production caller that
    /// reaches for a `_for_test` seam is exactly what convention 15's recipe
    /// split is meant to make impossible.
    pub fn clear_for_test(&self) {
        self.clear_for_identity_change();
    }

    /// Test-only MLS group bootstrap. Creates a new thread directly with the
    /// given participants (plus an implicit "self") and the MlsGroup flavor.
    /// Bypasses welcome / key-package distribution that the per-rail backend
    /// owns; suitable for setting up fixtures the e2e tests then mutate.
    pub fn create_mls_group(&self, participants: Vec<TypedAddress>) -> ThreadId {
        let key = ThreadKey::Participants {
            rail: Rail::FaunaMls,
            participants: participants.clone(),
        };
        let owner_named: Vec<ActorId> = participants
            .iter()
            .filter_map(|p| p.person_actor_id())
            .collect();
        let id = self
            .threads
            .find_or_create(key, participants, ThreadFlavor::MlsGroup, None);
        // Stands in for the owner creating the group, so the handles it seats
        // are the owner's own gesture — anchor-grade, as `send_new_thread`'s.
        self.threads.mark_anchor_grade(&id, owner_named);
        self.notify();
        id
    }

    /// Test-only backend bootstrap. Registers a `MockRailBackend` for every
    /// rail so `inject_inbound_for_test` can route inbound messages without
    /// the foreign caller having to construct backends across the FFI.
    ///
    /// The mail rail is told who the user is — [`TEST_SEAM_SELF_ADDRESS`], the
    /// address an injected message goes to by default — so reply-all drops it
    /// exactly as the real `SmtpBackend` drops the user's own address. Only
    /// mail: it is the one rail whose reply seeds recipients
    /// (`supports_recipient_selection`), and a FaunaMls self address would
    /// change what the mock's own-message and room paths do.
    pub fn install_mock_backends_for_test(&self) {
        use crate::backends::mock::MockRailBackend;
        for rail in [Rail::FaunaMls, Rail::Smtp, Rail::Bridged] {
            let backend = MockRailBackend::new(rail);
            if rail == Rail::Smtp {
                backend.set_self_address(TypedAddress::Email {
                    email_address: TEST_SEAM_SELF_ADDRESS.to_string(),
                });
            }
            self.register_backend(Arc::new(backend));
        }
    }

    /// Test-only: register a `MockRailBackend` for `rail` that knows who the
    /// user is — `self_address` — replacing whatever that rail's slot holds.
    ///
    /// [`Self::install_mock_backends_for_test`] leaves FaunaMls self-less on
    /// purpose (see its doc), which also means a FaunaMls-only gesture that must
    /// attribute itself — `toggle_reaction` resolves "me" off this rail's
    /// `self_address` — is silently dropped. A foreign unit pin of such a
    /// gesture (android's Robolectric agent pins) cannot build a mock across
    /// the FFI, so it opts in here, for the one rail it needs.
    pub fn install_mock_backend_knowing_self_for_test(
        &self,
        rail: Rail,
        self_address: TypedAddress,
    ) {
        use crate::backends::mock::MockRailBackend;
        let backend = MockRailBackend::new(rail);
        backend.set_self_address(self_address);
        self.register_backend(Arc::new(backend));
    }
}

/// The address the e2e test seam treats as the local user: the default
/// `recipient` of an injected message, and the mock mail rail's
/// `self_address`, so a reply-all on an injected mail drops it exactly as the
/// real `SmtpBackend` drops the user's own address. Where the app's mail rail
/// is the real one, the payload parser resolves it to that rail's own address.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
pub const TEST_SEAM_SELF_ADDRESS: &str = "me@self-nest.test";

// A plain (non-UniFFI-exported) impl block: `serde_json::Map` has no `LiftRef`
// impl, so this method cannot sit in the seam impl above even under the
// `uniffi::export` cfg_attr's own feature gate — Windows reaches the same
// dispatch through its own C# payload parse and never needs this signature.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
impl ConversationsManager {
    /// Test-only: pin where this run's news starts, so a Rust test states which
    /// of its messages are launch history and which arrived live instead of
    /// racing the clock the store reads at construction
    /// (`conversations.md` § State & data shape → *When a thread is read*).
    /// In this plain block because no app needs it: an e2e injection is stamped
    /// "now", which is already past the floor of the app it is injected into.
    pub fn set_launch_floor_for_test(&self, floor_ms: i64) {
        self.threads.set_launch_floor_for_test(floor_ms);
    }

    /// Test-only: parse a flat JSON test-seam payload into a
    /// [`RailInboundMessage`] and inject it — the shared parser behind the
    /// `conversations_inject_inbound` test-agent command, previously two
    /// hand-copies (tui, linux) whose own doc comments already called each
    /// other a mirror. Windows reaches the same dispatch
    /// ([`Self::inject_own_for_test`] etc.) through its own C# `TestAgent.cs`
    /// payload parse (it cannot call this Rust parser directly), so this
    /// collapses only the two Rust-native call sites.
    ///
    /// Payload keys (all optional, defaults noted): `rail` (default
    /// `"FaunaMls"`), `sender`, `recipient` (default
    /// [`TEST_SEAM_SELF_ADDRESS`], which on the mail rail resolves to the rail's
    /// real self address when it has one), `recipients` (a list naming the
    /// message's whole To/Cc set; wins over `recipient`), `subject`, `body`, `message_id` (default: a
    /// process-unique synthetic id — a monotonic counter, not a uuid dep, is
    /// unique enough for `in_reply_to` threading within one test run),
    /// `in_reply_to`, `force_subject_change`, `is_own`, `attachments`
    /// (`[{filename, mime_type?, data_base64}]`, cached via
    /// [`Self::make_attachment_for_test`] so the bubble resolves
    /// `dm-attachment-image`/`-file` exactly as a real inbound MIME parse
    /// would), `labels` (`[{category, confidence_per_mille}]`, staged via
    /// [`Self::inject_inbound_with_labels_for_test`] for the family-safety
    /// content-floor render — the generic inject path never classifies),
    /// `plane_record` (`{channel_id, envelope}`, resolved through
    /// [`crate::plane::plane_ref`] for the account-data-plane T1 reporter),
    /// and `legal_takedown_ref` (materializes the shared legal-takedown
    /// tombstone bubble without a real nest-side takedown). Malformed
    /// attachment/label entries are dropped silently (a test-only injector).
    pub fn inject_inbound_from_test_payload(
        &self,
        p: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), BackendError> {
        let rail = Rail::parse(p.get("rail").and_then(|v| v.as_str()).unwrap_or("FaunaMls"));
        // A bridged address names its bridge beside the far spelling
        // (`conversations.md` § Where logic lives → *The `Bridged` adapter*,
        // ruling 2 (c)); `bridge_id` is the key that carries it, and a bridged
        // inject without one has no address to build.
        let bridge_id = p.get("bridge_id").and_then(|v| v.as_str());
        let address = |raw: &str| -> Result<TypedAddress, BackendError> {
            match (rail, bridge_id) {
                (Rail::Bridged, Some(bridge)) => Ok(TypedAddress::unresolved_bridged(bridge, raw)),
                _ => TypedAddress::unresolved_for_rail(rail, raw).ok_or_else(|| {
                    BackendError::Internal(
                        "a Bridged inject names its bridge in `bridge_id`".to_string(),
                    )
                }),
            }
        };
        let sender_raw = p.get("sender").and_then(|v| v.as_str()).unwrap_or("");
        // `recipients` (a list — the message's whole To/Cc set) wins over the
        // single-recipient shorthand `recipient`; a mail thread's participants
        // are its recipients plus its sender, so this is how a test builds a
        // thread whose reply-all has more than one other person to seed.
        let recipients_raw: Vec<&str> = match p.get("recipients").and_then(|v| v.as_array()) {
            Some(list) => list.iter().filter_map(|v| v.as_str()).collect(),
            None => vec![
                p.get("recipient")
                    .and_then(|v| v.as_str())
                    .unwrap_or(TEST_SEAM_SELF_ADDRESS),
            ],
        };
        // The seam's placeholder for "you" names the local user, so on the mail
        // rail it is the rail's REAL self wherever the app has one — a signed-in
        // session registers the real `SmtpBackend` over the e2e mocks, and there
        // the placeholder would otherwise reach reply-all as a stranger. Mail only:
        // it is the one rail whose participants seed a reply's recipients.
        let mail_self = (rail == Rail::Smtp)
            .then(|| {
                self.backends
                    .read()
                    .unwrap()
                    .get(&rail)
                    .and_then(|b| b.self_address())
            })
            .flatten()
            .filter(|me| !me.display().is_empty());
        let subject = p
            .get("subject")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let body = p
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let message_id = p
            .get("message_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(next_test_message_id);
        let in_reply_to = p
            .get("in_reply_to")
            .and_then(|v| v.as_str())
            .map(|s| MessageId(s.to_string()));
        let force_subject_change = p
            .get("force_subject_change")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        // Force the injected message's `is_own` (the delete affordance + own-pill
        // highlight need an own bubble, which a real send can't produce on an
        // injected/groupless FaunaMls thread — see `inject_own_for_test`).
        let is_own = p.get("is_own").and_then(|v| v.as_bool()).unwrap_or(false);

        // Renderable inbound attachments: each entry carries base64 bytes; cached
        // under their content-addressed `blob_hash` so the bubble resolves
        // `dm-attachment-image` / `-file` exactly as a real inbound MIME parse
        // would. Absent / malformed entries are dropped silently.
        let attachments = p
            .get("attachments")
            .and_then(|v| v.as_array())
            .map(|arr| {
                use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
                arr.iter()
                    .filter_map(|a| {
                        let o = a.as_object()?;
                        let filename = o.get("filename").and_then(|v| v.as_str())?.to_string();
                        let mime_type = o
                            .get("mime_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("application/octet-stream")
                            .to_string();
                        let bytes = B64.decode(o.get("data_base64")?.as_str()?).ok()?;
                        Some(self.make_attachment_for_test(filename, mime_type, bytes))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Explicit content labels: `[{category, confidence_per_mille}]`. The
        // generic inject path doesn't classify (only the real MLS receive path
        // does), so a test that needs a *labeled* bubble stages the labels here,
        // the same shape as the feed's `TestPostSpec.labels`.
        let labels: Vec<fauna_core::content_category::ContentLabelEntry> = p
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| {
                        let o = l.as_object()?;
                        Some(fauna_core::content_category::ContentLabelEntry {
                            category: o.get("category")?.as_str()?.to_string(),
                            confidence_per_mille: u16::try_from(
                                o.get("confidence_per_mille")?.as_u64()?,
                            )
                            .ok()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let msg = RailInboundMessage {
            rail,
            sender: address(sender_raw)?,
            recipients: recipients_raw
                .into_iter()
                .map(|r| match &mail_self {
                    Some(me) if r == TEST_SEAM_SELF_ADDRESS => Ok(me.clone()),
                    _ => address(r),
                })
                .collect::<Result<_, _>>()?,
            subject,
            body,
            // FaunaMls carries markdown; every other rail is stamped PlainText via
            // this synthetic injector (a real inbound HTML mail goes through the
            // real parse path, which stamps Markdown — the html-mail render path
            // is proven only by the real-SMTP tier_3 e2e, never this seam).
            body_format: match rail {
                Rail::FaunaMls => BodyFormat::Markdown,
                _ => BodyFormat::PlainText,
            },
            timestamp_ms: fauna_core::data::Timestamp::now_millis_or_zero() as i64,
            message_id: MessageId(message_id),
            in_reply_to,
            attachments,
            badges: MessageBadges::default(),
            // `Some(reference)` materializes the shared legal-takedown tombstone
            // bubble without a real nest-side takedown (`moderation.md` §
            // Legal takedown).
            legal_takedown_ref: p
                .get("legal_takedown_ref")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            // The record's account-data-plane identity, which production derives
            // in `poll_inbound_conv` from the sealed envelope this synthetic seam
            // never has (`account-data-plane.md` § The replica boundary → T1).
            // Given a `channel_id` + `envelope`, the seam runs the SAME shared
            // derivation the real path does — it fakes the *envelope*, never the
            // rule — so a test can drive the T1 reporter end to end.
            plane_ref: p.get("plane_record").and_then(|v| {
                crate::plane::plane_ref(
                    v.get("channel_id")?.as_str()?,
                    v.get("envelope")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .as_bytes(),
                )
            }),
        };

        match (is_own, force_subject_change) {
            (true, _) => self.inject_own_for_test(msg),
            (false, Some(s)) => self.inject_inbound_with_subject_change_for_test(msg, s),
            // The labeled variant only exists for the plain inbound path; an own
            // message and a subject-change inject have no content-floor arm to
            // drive, so they keep their own seams and ignore `labels`.
            (false, None) if !labels.is_empty() => {
                self.inject_inbound_with_labels_for_test(msg, labels)
            }
            (false, None) => self.inject_inbound_for_test(msg),
        }
    }
}

/// The actor id a test fixture should give a `TypedAddress::Fauna` built from
/// `handle` — deterministic, and **distinct per handle**.
///
/// It exists because the obvious placeholder is a trap that has already been
/// shipped: every app's `create_mls_group` e2e seam minted its participants with
/// `ActorId([0u8; 32])`, so a two-member fixture had two members with **one
/// identity**. That is invisible to any assertion about display text — chips
/// render handles — and silently fatal to every assertion about a *person*: a
/// per-member flag keyed on the actor id lights up on all of them at once, and a
/// test pinning "only the flagged member is marked" cannot fail for the right
/// reason because its fixture cannot represent two people.
///
/// Deterministic so a caller can compute the same id (`blake3(handle)`) and
/// address a specific member; hashed rather than enumerated so the value does
/// not depend on the order participants were passed in.
///
/// **A free export, not an associated fn, and not an unexported one.** Free
/// because uniffi's `export` accepts only methods and constructors on an impl
/// block ("associated functions are not currently supported") — declaring it
/// beside its callers in the seam impl above took every apple and windows build
/// red for a gate cycle on 2026-08-09 while the Linux and web builds stayed
/// green, since the pair that fails (`uniffi` + `test-helpers`) is two
/// non-default features neither of those two turns on together.
/// `rail_glyph` in `address.rs` is the same shape for the same reason.
///
/// **Exported** because the six apps that still mint the all-zero placeholder
/// need this value in *their* seam handler, which is where the id is minted:
/// macos/ios build `.fauna(handle:actorId:)` in Swift, android in Kotlin,
/// windows in C#, and none of those three languages has blake3 — so an
/// unexported helper does not merely inconvenience them, it makes the fix
/// unbuildable without a per-platform hash dependency, which is the divergence
/// priority #2 exists to prevent. (web and linux consume it as a Rust crate dep
/// and would not have needed the export.) It carries the seam impl's two cfg
/// gates verbatim — see that block's comment for why visibility keys on the
/// profile while the FFI face keys on the feature only.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[cfg_attr(all(feature = "uniffi", feature = "test-helpers"), uniffi::export)]
pub fn test_actor_id_for_handle(handle: &str) -> ActorId {
    ActorId(*blake3::hash(handle.as_bytes()).as_bytes())
}

/// Upper bound on the plaintext characters carried in a reply-quote snippet — a
/// payload bound, not the visual cap (each app clamps the rendered quote to
/// ≤ 2 lines). Generous enough that a 2-line clamp never runs out of text.
const REPLY_QUOTE_SNIPPET_MAX_CHARS: usize = 240;

/// The display name shown as a reply-quote's author: the parent's
/// `sender_display` when set, else its address display (`sender_display` is empty
/// at ingest on several rails — see the bucket constructors).
fn reply_quote_author(parent: &MessageSnapshot) -> String {
    if parent.sender_display.is_empty() {
        parent.sender.display()
    } else {
        parent.sender_display.clone()
    }
}

/// A plaintext preview of a parent body for a reply-quote, derived the same way
/// the thread-list snippet is (`store::threads::summarize` →
/// `markdown_to_plaintext`) so the two previews agree, then char-bounded.
fn reply_quote_snippet(body: &str) -> String {
    let plain = fauna_core::markdown::markdown_to_plaintext(body);
    if plain.chars().count() <= REPLY_QUOTE_SNIPPET_MAX_CHARS {
        plain
    } else {
        plain.chars().take(REPLY_QUOTE_SNIPPET_MAX_CHARS).collect()
    }
}

/// Project an in-bubble reply-quote (render-model.md § D2 `QuotedMessage`) onto
/// each reply in a thread's loaded message list. For every message whose
/// `reply_to` resolves to a parent **loaded in this same list**, prepend a
/// [`fauna_core::render::RenderBlock::QuotedMessage`] (the parent's author + a
/// plaintext snippet) as the FIRST block, so the in-order client walkers paint it
/// above the body. A reply whose parent isn't loaded shows no quote (we hold only
/// the bare id). Read-time only — never persisted back to the store; idempotent
/// (strips any pre-existing leading quote first) so re-projection is safe.
fn fold_reply_quotes(messages: &mut [MessageSnapshot]) {
    use fauna_core::render::RenderBlock;
    // Parent preview lookup: id → (author_display, snippet) for every loaded
    // message. Built before the mutable walk (owned clones, so the immutable
    // borrow ends here).
    let previews: HashMap<MessageId, (String, String)> = messages
        .iter()
        .map(|m| {
            (
                m.message_id.clone(),
                (reply_quote_author(m), reply_quote_snippet(&m.body)),
            )
        })
        .collect();
    for m in messages.iter_mut() {
        // Idempotent: drop a prior leading reply-quote before re-projecting.
        if matches!(
            m.document.blocks.first(),
            Some(RenderBlock::QuotedMessage { .. })
        ) {
            m.document.blocks.remove(0);
        }
        if let Some(parent_id) = &m.reply_to
            && let Some((author_display, snippet)) = previews.get(parent_id)
        {
            m.document.blocks.insert(
                0,
                RenderBlock::QuotedMessage {
                    author_display: author_display.clone(),
                    snippet: snippet.clone(),
                },
            );
        }
    }
}
