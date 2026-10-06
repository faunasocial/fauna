use crate::address::{Rail, TypedAddress};
use crate::backend::MailFeed;
use crate::compose::ComposeState;
use crate::keying::ThreadKey;
use crate::message::{BodyFormat, MessageId, MessageSnapshot, attachment_blocks};
use crate::snapshot::{ThreadDetail, ThreadSummary};
use crate::thread::{ThreadFlavor, ThreadId};
use fauna_core::identity::ActorId;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

#[derive(Default)]
pub struct ThreadStore {
    inner: RwLock<Inner>,
}

#[cfg(test)]
thread_local! {
    /// Counts [`ThreadStore::get`]'s full-detail clones, so a reader's cost
    /// can be asserted as a COUNT rather than a duration
    /// (`e2e-latency-independent-assertions.md` — convention 14).
    static FULL_CLONES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// [`ThreadStore::get`] clones counted on this thread so far. Test-only.
#[cfg(test)]
pub(crate) fn full_clones() -> u64 {
    FULL_CLONES.with(std::cell::Cell::get)
}

#[derive(Default)]
struct Inner {
    threads: HashMap<ThreadId, ThreadDetail>,
    by_key: HashMap<ThreadKey, ThreadId>,
    by_message: HashMap<crate::message::MessageId, ThreadId>,
    /// The parse facts of each message appended through
    /// [`ThreadStore::append_inbound_message`], for
    /// [`ThreadStore::mark_message_own`]'s compare.
    inbound_parse: HashMap<MessageId, InboundParseFacts>,
    /// Per thread, the Fauna participants whose handle **the owner's own
    /// gesture** put on the row — a recipient typed and accepted at compose, a
    /// member the owner added — and the handle that gesture named. The
    /// provenance a succession's tier-2 anchor reads
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*): a handle's `@domain` is a dial
    /// target, so only a handle no other party chose may say which nest the
    /// witness asks for a peer's chain. A name served by the room home
    /// ([`ThreadStore::name_participants`]) or copied off another thread at
    /// seat time is display only and is never marked.
    ///
    /// Positive set on purpose — a row is anchor-grade only when a writer said
    /// so, so a future seat site that forgets to mark fails safe (display
    /// only), never open. The mark records the handle the owner named rather
    /// than a bare flag so a departed-and-re-admitted member whose re-seated
    /// row carries a *different* name reads display-only: the read
    /// ([`ThreadStore::anchor_grade_handle_for`]) honours a mark only while
    /// the live row still carries exactly that handle, which is also why a
    /// removal need not sweep it (the optimistic remove's rollback re-seats
    /// the same row and inherits the mark for free). Kept off
    /// [`ThreadDetail`] because that is a UniFFI record every app renders and
    /// no app renders provenance; persisted per channel by
    /// [`crate::store::history::ChannelHistorySlice::anchor_grade_handles`].
    anchor_grade: HashMap<ThreadId, HashMap<ActorId, String>>,
    /// Per thread, the messages the user has not read — what
    /// [`ThreadSummary::unread_count`] counts (`conversations.md` § State &
    /// data shape → *When a thread is read*). A set of ids rather than a
    /// position on purpose: the carriers that sync read state across the
    /// account's devices key it per rail (a channel `seq`, a mailbox flag), and
    /// no one ordering key exists to hold here — each carrier fills this set
    /// (`conversation-read-state.md` § How the carriers meet the in-memory
    /// set). An id whose message was evicted, deleted or upgraded to own simply
    /// stops counting ([`summarize`]), so no removal path has to sweep it.
    unread: HashMap<ThreadId, HashSet<MessageId>>,
    /// The fauna-native rail's read positions, per channel (lowercase hex):
    /// every message at or below `through` is read
    /// (`conversation-read-state.md` § The read-marker record). Joined by
    /// `max` from two sources — what the account store delivers
    /// ([`ThreadStore::apply_read_positions`]) and this device's own reads
    /// ([`ThreadStore::raise_read_position`]) — so a delivery that predates a
    /// local read can never move a thread backwards.
    read_positions: HashMap<String, u64>,
    /// Whether the positions above are **known** — the seam has delivered them
    /// at least once this run. Until then a native message is judged by the
    /// launch floor like every other ([`Inner::is_news`]): an app between
    /// launch and its account-store-ready edge, and web for its whole run,
    /// cannot tell history from news any other way.
    positions_known: bool,
    /// Per held `INBOX` mail message, its UID on the nest — the identity the
    /// mail rail's read marker, IMAP `\Seen`, is written and delivered under
    /// (`conversation-read-state.md` § Mail: `\Seen` is the marker), and
    /// [`Self::by_inbox_uid`] its reverse. Filled only through a
    /// [`MailArrival`] naming `INBOX`; a `Sent` copy's UID numbers another
    /// mailbox and is never kept.
    inbox_uid: HashMap<MessageId, u32>,
    by_inbox_uid: HashMap<u32, MessageId>,
    /// The **launch floor** — where this run's news starts ([`ThreadStore::new`];
    /// `conversations.md` § State & data shape → *When a thread is read*). The
    /// store is refilled from the nest at every launch, and nothing yet tells a
    /// replayed message the user read last week from one that arrived while the
    /// app was closed — so a message stamped before the floor is history: it
    /// never enters [`Self::unread`], and the new-message banner decision reads
    /// the same floor off the snapshot (`ConversationsSnapshot::launch_floor_ms`,
    /// `notification.rs`) so a thread a slower rail delivers after the seed never
    /// banners for old mail. Under-reports by design until read state syncs: a
    /// missing badge is a gap, a false one on every old mail after every restart
    /// is a lie. Since synced read state landed it is the unread rule's
    /// **position-unknown arm** only ([`Inner::is_news`]) — a native message
    /// before the positions arrive (mail never reads it: its `\Seen` flag is
    /// its marker, [`Inner::append`]); that read goes when the last
    /// host without an account runtime gains one (`conversation-read-state.md`
    /// § web). The banner's read stays — a banner is for what arrives while the
    /// app runs, and that question has a run-start answer whatever read state
    /// knows.
    launch_floor_ms: i64,
    next_id: u64,
}

impl Inner {
    /// Whether a not-own `msg` arriving now is unread
    /// (`conversation-read-state.md` § How the carriers meet the in-memory
    /// set). A native message whose channel position is known is unread iff
    /// its nest-assigned `seq` is above the position — no clock anywhere.
    /// Everything else falls back to the launch floor: a message stamped
    /// before this run began is history. Mail never asks — its `\Seen` flag
    /// decides ([`Self::append`]).
    fn is_news(&self, msg: &MessageSnapshot) -> bool {
        match msg.message_id.channel_position() {
            Some((channel_hex, seq)) if self.positions_known => seq > self.position_of(channel_hex),
            _ => msg.timestamp_ms >= self.launch_floor_ms,
        }
    }

    /// The read position of the channel `channel_hex` names; an absent entry
    /// is `0` — nothing read.
    fn position_of(&self, channel_hex: &str) -> u64 {
        self.read_positions
            .get(&channel_hex.to_ascii_lowercase())
            .copied()
            .unwrap_or(0)
    }

    /// Append `msg` to thread `id`; whether it was actually pushed. `mail` is
    /// what the inbound mail record said beyond the snapshot, when one did.
    fn append(&mut self, id: &ThreadId, msg: MessageSnapshot, mail: Option<MailArrival>) -> bool {
        self.by_message.insert(msg.message_id.clone(), id.clone());
        let position_or_floor_news = self.is_news(&msg);
        let Some(detail) = self.threads.get_mut(id) else {
            return false;
        };
        // Last-line dedup: a thread must never show the same `message_id`
        // twice, regardless of which append path reached here. The id-level
        // guard in `ConversationsManager::ingest_inbound` already suppresses
        // the local-echo-vs-server-Sent-copy double for the *mail* rail, but
        // it is the ONLY dedup the FaunaMls path
        // (`ConversationsManager::ingest_inbound_to_thread`) and any future
        // append caller have — so enforce the invariant at the store, the one
        // chokepoint every append crosses. RFC 5322 `Message-ID`s are globally
        // unique (and FaunaMls ids are channel-message unique), so a same-id
        // append is always a duplicate of a message already in the thread:
        // skip it, the first copy stands.
        if detail
            .messages
            .iter()
            .any(|m| m.message_id == msg.message_id)
        {
            return false;
        }
        // `MessageSnapshot.document` is already the *complete* tree every reader
        // paints — the text body plus `Attachment` blocks in body order — because
        // each construction site produces it via `document_for_message`
        // (render-model.md § D1/D2: embeds are first-class blocks, not sibling
        // fields). The store just dedups and pushes.
        //
        // Every arrival is unread here, the open thread's included: the store
        // does not know what the user is looking at. The manager reads the
        // attended thread before any observer sees the change
        // (`ConversationsManager::notify`).
        //
        // Which arrivals are news is the rail's carrier's to say
        // (`conversation-read-state.md` § How the carriers meet the in-memory
        // set). Mail's is the message's own `\Seen` flag, so the launch floor
        // never applies to it: a mail that arrived while every app was closed
        // lacks the flag and is unread at launch. A mail message no record
        // described (a local echo is own; a test injection) lacks the flag too.
        let news = !msg.is_own
            && if detail.rail == Rail::Smtp {
                !mail.is_some_and(|m| m.has_seen_flag)
            } else {
                position_or_floor_news
            };
        if news {
            self.unread
                .entry(id.clone())
                .or_default()
                .insert(msg.message_id.clone());
        }
        if let Some(m) = mail
            && m.mailbox == MailFeed::Inbox
        {
            self.inbox_uid.insert(msg.message_id.clone(), m.uid);
            self.by_inbox_uid.insert(m.uid, msg.message_id.clone());
        }
        detail.messages.push(msg);
        true
    }

    /// Drop `msg_id`'s `INBOX` UID mapping, both directions.
    fn forget_inbox_uid(&mut self, msg_id: &MessageId) {
        if let Some(uid) = self.inbox_uid.remove(msg_id) {
            self.by_inbox_uid.remove(&uid);
        }
    }
}

/// What an inbound mail record says about its message beyond the
/// [`MessageSnapshot`]: which mailbox and UID it rests under on the nest, and
/// whether it carries IMAP `\Seen`
/// ([`crate::backend::InboundMailRecord::has_seen_flag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MailArrival {
    pub mailbox: MailFeed,
    pub uid: u32,
    pub has_seen_flag: bool,
}

/// What an inbound message parsed to beyond what its [`MessageSnapshot`]
/// holds: the recipients and subject that route it into a thread, and the
/// text format its body renders in. Kept beside the held copy only so
/// [`ThreadStore::mark_message_own`] can compare a later `Sent` copy's parse
/// field by field. They stay off the snapshot because it is a UniFFI record
/// every app binds, and no app reads these per message.
#[derive(Clone, Debug, PartialEq)]
pub struct InboundParseFacts {
    pub recipients: Vec<TypedAddress>,
    pub subject: Option<String>,
    pub body_format: BodyFormat,
}

/// What [`ThreadStore::mark_message_own`] found when a `Sent` copy collided
/// with the message held under its Message-ID — the Message-ID collision rule
/// of `docs/goal/ui/conversations.md:1572`, decided field by field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentCopyOutcome {
    /// The held copy parsed to the same message and is now own, showing the
    /// `Sent` copy's delivery time. Nothing else to do.
    Upgraded,
    /// The held copy was own already — the sending device's local echo, or an
    /// earlier `Sent` copy. The duplicate is dropped.
    AlreadyOwn,
    /// The held copy differs from the `Sent` copy in at least one compared
    /// field: a squat on the account's Message-ID. The caller displaces it —
    /// [`ThreadStore::evict_message`], then the ordinary ingest of the `Sent`
    /// copy.
    Squat,
    /// A held not-own copy with no parse facts to compare against — only a
    /// non-inbound append leaves one, and no rail's collision path reaches
    /// such a message today. Left as it is, like the pre-compare dedup did.
    NotComparable,
    /// No message is held under the id.
    NotHeld,
}

/// What [`ThreadStore::evict_message`] removed: the thread the message left,
/// and whether that left the thread empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictedMessage {
    pub thread_id: ThreadId,
    pub emptied: bool,
}

impl ThreadStore {
    pub fn new() -> Self {
        let store = Self::default();
        store.inner.write().unwrap().launch_floor_ms = launch_floor_ms();
        store
    }

    /// The launch floor this run's news starts at (`Inner::launch_floor_ms`),
    /// published on every snapshot for the banner decision.
    pub fn launch_floor_ms(&self) -> i64 {
        self.inner.read().unwrap().launch_floor_ms
    }

    /// Pin where this run's news starts — tests state the floor instead of
    /// racing the clock [`Self::new`] reads.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_launch_floor_for_test(&self, floor_ms: i64) {
        self.inner.write().unwrap().launch_floor_ms = floor_ms;
    }

    pub fn list_summaries(&self) -> Vec<ThreadSummary> {
        let inner = self.inner.read().unwrap();
        let mut out: Vec<_> = inner
            .threads
            .values()
            .map(|detail| summarize(detail, inner.unread.get(&detail.thread_id)))
            .collect();
        out.sort_by_key(|s| -s.last_activity_ms);
        out
    }

    /// The user has thread `id` open (or said so): nothing in it is unread any
    /// more. Whether that changed anything — so a caller on the notify path can
    /// tell a read from a no-op.
    pub fn mark_read(&self, id: &ThreadId) -> bool {
        // The read lock first: this runs on every notify for the attended
        // thread, and almost every one of those finds nothing to clear.
        if !self.inner.read().unwrap().unread.contains_key(id) {
            return false;
        }
        self.inner.write().unwrap().unread.remove(id).is_some()
    }

    /// Raise this device's read position for the channel thread `id` holds
    /// to the highest channel `seq` among its messages, and say which — the
    /// `(channel_hex, through)` the read marker is raised to
    /// (`conversation-read-state.md` § The read-marker record → *Who writes
    /// it*). `None` when the thread holds no fauna-native message.
    ///
    /// The in-memory raise is what keeps a later delivery honest: positions the
    /// store delivers join by `max`, so one that predates this raise reaching
    /// the store cannot re-present the thread as unread.
    pub fn raise_read_position(&self, id: &ThreadId) -> Option<(String, u64)> {
        let mut inner = self.inner.write().unwrap();
        let (channel_hex, through) = inner
            .threads
            .get(id)?
            .messages
            .iter()
            .filter_map(|m| m.message_id.channel_position())
            .max_by_key(|(_, seq)| *seq)
            .map(|(hex, seq)| (hex.to_ascii_lowercase(), seq))?;
        let held = inner.read_positions.entry(channel_hex.clone()).or_default();
        *held = (*held).max(through);
        Some((channel_hex, through))
    }

    /// Take the native rail's read positions from the account store —
    /// `(channel_hex, through)` pairs, an absent channel meaning `0`
    /// (`conversation-read-state.md` § How the carriers meet the in-memory
    /// set → *Native, position known*). The positions join this device's own
    /// by `max`, and from here on they, not the launch floor, decide a native
    /// message: every thread holding one has its unread set **recomputed
    /// whole** as `{ not own, not deleted, seq > through }`, which is both the
    /// first delivery's fill and a later raise from another device removing
    /// what it covers. Mail threads are untouched. Whether any thread's
    /// unread set changed.
    pub fn apply_read_positions(&self, positions: impl IntoIterator<Item = (String, u64)>) -> bool {
        let mut guard = self.inner.write().unwrap();
        let inner = &mut *guard;
        for (channel_hex, through) in positions {
            let held = inner
                .read_positions
                .entry(channel_hex.to_ascii_lowercase())
                .or_default();
            *held = (*held).max(through);
        }
        inner.positions_known = true;
        let mut changed = false;
        for (id, detail) in &inner.threads {
            let mut native = false;
            let mut unread = HashSet::new();
            for m in &detail.messages {
                let Some((channel_hex, seq)) = m.message_id.channel_position() else {
                    continue;
                };
                native = true;
                let through = inner
                    .read_positions
                    .get(&channel_hex.to_ascii_lowercase())
                    .copied()
                    .unwrap_or(0);
                if !m.is_own && !m.deleted && seq > through {
                    unread.insert(m.message_id.clone());
                }
            }
            if !native {
                continue;
            }
            let unchanged = inner
                .unread
                .get(id)
                .map_or(unread.is_empty(), |held| *held == unread);
            if unchanged {
                continue;
            }
            changed = true;
            if unread.is_empty() {
                inner.unread.remove(id);
            } else {
                inner.unread.insert(id.clone(), unread);
            }
        }
        changed
    }

    /// The `INBOX` UIDs of thread `id`'s unread mail messages, ascending — what
    /// reading the thread must set `\Seen` on (`conversation-read-state.md`
    /// § Mail: `\Seen` is the marker → *Writing it*). Taken before
    /// [`Self::mark_read`] empties the set. Own messages are never unread, so
    /// no `Sent` copy is ever named.
    pub fn unread_inbox_uids(&self, id: &ThreadId) -> Vec<u32> {
        let inner = self.inner.read().unwrap();
        let (Some(ids), Some(detail)) = (inner.unread.get(id), inner.threads.get(id)) else {
            return Vec::new();
        };
        let mut uids: Vec<u32> = detail
            .messages
            .iter()
            .filter(|m| !m.is_own && !m.deleted && ids.contains(&m.message_id))
            .filter_map(|m| inner.inbox_uid.get(&m.message_id).copied())
            .collect();
        uids.sort_unstable();
        uids
    }

    /// Apply one delivered `\Seen` state to the held `INBOX` message at `uid`
    /// (`conversation-read-state.md` § Mail → *Learning of a change made
    /// elsewhere*): gaining the flag leaves the unread set, losing it re-enters
    /// — a mail client's "mark as unread". Whether the set changed; `false` for
    /// a UID this run holds no message under, or an own or deleted message.
    pub fn apply_inbox_seen(&self, uid: u32, has_seen_flag: bool) -> bool {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        let Some(message_id) = inner.by_inbox_uid.get(&uid) else {
            return false;
        };
        let Some(thread_id) = inner.by_message.get(message_id) else {
            return false;
        };
        if has_seen_flag {
            let Some(ids) = inner.unread.get_mut(thread_id) else {
                return false;
            };
            let removed = ids.remove(message_id);
            if ids.is_empty() {
                inner.unread.remove(thread_id);
            }
            return removed;
        }
        let countable = inner.threads.get(thread_id).is_some_and(|detail| {
            detail
                .messages
                .iter()
                .any(|m| &m.message_id == message_id && !m.is_own && !m.deleted)
        });
        countable
            && inner
                .unread
                .entry(thread_id.clone())
                .or_default()
                .insert(message_id.clone())
    }

    /// Whether thread `id`'s latest message, as full plaintext, contains
    /// `needle_lower` (already lowercased) — the list filter's body match, read under
    /// the store lock so no `ThreadDetail` is cloned. `false` for an unknown thread.
    pub fn latest_plaintext_matches(&self, id: &ThreadId, needle_lower: &str) -> bool {
        self.inner
            .read()
            .unwrap()
            .threads
            .get(id)
            .is_some_and(|detail| latest_plaintext_contains(detail, needle_lower))
    }

    /// Thread `id` in full — every message's body, rendered document and
    /// attachments included. The render path wants all of that; a reader that
    /// only needs the thread's facts wants [`Self::get_without_messages`].
    pub fn get(&self, id: &ThreadId) -> Option<ThreadDetail> {
        #[cfg(test)]
        FULL_CLONES.with(|n| n.set(n.get() + 1));
        self.inner.read().unwrap().threads.get(id).cloned()
    }

    /// Thread `id` with its message list left EMPTY (and no compose state),
    /// plus the two facts a state reader takes from those messages — how many
    /// there are, and each one's subject line — gathered under the store lock,
    /// so nothing heavier than those strings is cloned.
    ///
    /// Exists for the e2e state serializer
    /// (`crate::state_json::conversation_threads_json`), which every app's
    /// state provider calls on every publish. Through [`Self::get`] that read
    /// deep-cloned every message of every thread, and a mailbox holding one
    /// multi-megabyte inbound message made each publish cost a quarter of a
    /// second on the UI thread — twenty times a second, so the thread never
    /// went idle (`apps/linux.md` § Message Flow). The fields are listed, not
    /// struct-updated from a clone, so a new `ThreadDetail` field is a compile
    /// error here rather than a silently copied body.
    pub fn get_without_messages(
        &self,
        id: &ThreadId,
    ) -> Option<(ThreadDetail, usize, Vec<String>)> {
        let inner = self.inner.read().unwrap();
        let d = inner.threads.get(id)?;
        let subject_lines = d
            .messages
            .iter()
            .map(|m| m.subject_line.clone().unwrap_or_default())
            .collect();
        let without = ThreadDetail {
            guardian_state: None,
            thread_id: d.thread_id.clone(),
            rail: d.rail,
            glyph: d.glyph,
            flavor: d.flavor.clone(),
            label: d.label.clone(),
            participants: d.participants.clone(),
            participant_displays: d.participant_displays.clone(),
            capabilities: d.capabilities,
            messages: Vec::new(),
            compose: ComposeState::default(),
            selected_message_id: None,
            room: d.room.clone(),
            bridge: d.bridge.clone(),
        };
        Some((without, d.messages.len(), subject_lines))
    }

    /// The bridge a [`crate::address::Rail::Bridged`] thread rides — its first
    /// bridged participant's `bridge_id` — read under the lock without cloning
    /// a message, for the manager's per-summary identity projection.
    pub fn bridge_id_of(&self, id: &ThreadId) -> Option<String> {
        let inner = self.inner.read().unwrap();
        let d = inner.threads.get(id)?;
        crate::backends::bridged::bridge_id_of(&d.participants).map(str::to_string)
    }

    /// Thread `id`'s participants, read under the lock without cloning a
    /// message — for the manager's per-summary guardian-state projection,
    /// which the rail answers from the participants.
    pub fn participants_of(&self, id: &ThreadId) -> Option<Vec<TypedAddress>> {
        let inner = self.inner.read().unwrap();
        inner.threads.get(id).map(|d| d.participants.clone())
    }

    /// Find or create a thread for the given lookup key.
    ///
    /// `label_override`, when `Some`, supplies the display label — used for
    /// subject-keyed threads to preserve the original-case subject (e.g.
    /// "Q4 budget") even though the key uses the normalized lowercase form
    /// for matching. When `None`, the label is derived from the key.
    pub fn find_or_create(
        &self,
        key: ThreadKey,
        participants: Vec<TypedAddress>,
        flavor: ThreadFlavor,
        label_override: Option<String>,
    ) -> ThreadId {
        let mut inner = self.inner.write().unwrap();
        if let Some(id) = inner.by_key.get(&key) {
            return id.clone();
        }
        inner.next_id += 1;
        let id = ThreadId(format!("t-{}", inner.next_id));
        let rail = match &key {
            ThreadKey::Participants { rail, .. } => *rail,
            ThreadKey::SubjectKeyed { rail, .. } => *rail,
            ThreadKey::Channel { rail, .. } => *rail,
            ThreadKey::ByMessageReference(_) => participants
                .first()
                .and_then(|p| p.rail())
                .unwrap_or(crate::address::Rail::FaunaMls),
        };
        let label = label_override.unwrap_or_else(|| match &key {
            ThreadKey::SubjectKeyed { subject, .. } => subject.clone(),
            _ => participants
                .iter()
                .map(|p| p.display())
                .collect::<Vec<_>>()
                .join(", "),
        });
        let detail = ThreadDetail {
            thread_id: id.clone(),
            rail,
            glyph: rail.glyph(),
            flavor: flavor.clone(),
            label,
            participant_displays: participants.iter().map(|p| p.display()).collect(),
            participants,
            capabilities: crate::capabilities::derive_capabilities(rail, flavor),
            messages: Vec::new(),
            compose: Default::default(),
            // Stored threads never carry a selection: it is manager state,
            // re-resolved onto every `thread_detail` emit.
            selected_message_id: None,
            // Nor a room or a bridge identity: the rail projects both onto
            // every emit.
            room: None,
            bridge: None,
            guardian_state: None,
        };
        inner.threads.insert(id.clone(), detail);
        inner.by_key.insert(key, id.clone());
        id
    }

    pub fn append_message(&self, id: &ThreadId, msg: MessageSnapshot) {
        self.inner.write().unwrap().append(id, msg, None);
    }

    /// [`Self::append_message`] for an inbound message, keeping what it parsed
    /// to beside it under the same lock ([`InboundParseFacts`]), so a later
    /// `Sent` copy of the same message can be compared against the whole parse
    /// ([`Self::mark_message_own`]). The facts are kept only when the message
    /// is actually appended. `mail` is what a mail record said of it beyond
    /// the snapshot — its read flag decides whether it is unread, and an
    /// `INBOX` UID is kept for the flag's write and delivery.
    pub fn append_inbound_message(
        &self,
        id: &ThreadId,
        msg: MessageSnapshot,
        facts: InboundParseFacts,
        mail: Option<MailArrival>,
    ) {
        let mut inner = self.inner.write().unwrap();
        let message_id = msg.message_id.clone();
        if inner.append(id, msg, mail) {
            inner.inbound_parse.insert(message_id, facts);
        }
    }

    pub fn thread_for_message(&self, msg_id: &crate::message::MessageId) -> Option<ThreadId> {
        self.inner.read().unwrap().by_message.get(msg_id).cloned()
    }

    /// Whether thread `id` itself holds a message under `msg_id` — the
    /// **thread-local** question, where [`Self::thread_for_message`] answers
    /// the store-wide one. `by_message` names one thread per id (the last to
    /// append it), so it settles this in O(1) both when it names `id` and when
    /// it names nobody; only an id some OTHER thread also appended costs the
    /// scan. A reader deciding whether a channel's own record was already
    /// folded needs this form: an id held elsewhere says nothing about the
    /// channel whose log mints it.
    pub fn thread_holds_message(&self, id: &ThreadId, msg_id: &crate::message::MessageId) -> bool {
        let inner = self.inner.read().unwrap();
        match inner.by_message.get(msg_id) {
            None => false,
            Some(holder) if holder == id => true,
            Some(_) => inner
                .threads
                .get(id)
                .is_some_and(|d| d.messages.iter().any(|m| m.message_id == *msg_id)),
        }
    }

    /// The decrypted plaintext body of one retained message, by id. The
    /// moderation train-correction path reads this: a **local detection**'s
    /// `content_id` is the message id of a post-decrypt classified message
    /// (`ConversationsManager::observe_local_detection`), and correcting it as
    /// ham feeds the tier-1 spam model with the *same text* the classifier saw —
    /// text only the client holds (MLS-sealed at rest). `None` once the message
    /// has aged out of the store.
    pub fn message_body(&self, msg_id: &crate::message::MessageId) -> Option<String> {
        let inner = self.inner.read().unwrap();
        let thread_id = inner.by_message.get(msg_id)?;
        inner
            .threads
            .get(thread_id)?
            .messages
            .iter()
            .find(|m| &m.message_id == msg_id)
            .map(|m| m.body.clone())
    }

    /// The thread that holds `msg_id` **and** that message's plaintext body, in
    /// one read-lock.
    ///
    /// The local search arm's projection needs exactly this pair for every hit
    /// (`fauna_client_index::local_search`): the sealed index stores postings
    /// only, so the snippet renders from the body, and the navigation target is
    /// the holding thread. Taking [`Self::thread_for_message`] and
    /// [`Self::message_body`] separately would lock twice per hit and could
    /// observe a message moving between them.
    pub fn locate_message(&self, msg_id: &crate::message::MessageId) -> Option<(ThreadId, String)> {
        let inner = self.inner.read().unwrap();
        let thread_id = inner.by_message.get(msg_id)?;
        let body = inner
            .threads
            .get(thread_id)?
            .messages
            .iter()
            .find(|m| &m.message_id == msg_id)
            .map(|m| m.body.clone())?;
        Some((thread_id.clone(), body))
    }

    /// Add `addr` to the thread's roster, idempotently.
    ///
    /// The already-present test is [`TypedAddress::same_participant`], not
    /// `display()`: two Fauna members may wear the same handle (an MLS roster's
    /// handles are empty by construction and attacker-chosen by threat model),
    /// and comparing the handle refuses the second of them — which on a roster
    /// of empty-handled members is *every* member after the first.
    pub fn add_participant_to(&self, id: &ThreadId, addr: TypedAddress) {
        let mut inner = self.inner.write().unwrap();
        if let Some(detail) = inner.threads.get_mut(id)
            && !detail
                .participants
                .iter()
                .any(|p| p.same_participant(&addr))
        {
            detail.participant_displays.push(addr.display());
            detail.participants.push(addr);
        }
    }

    /// Drop `addr` from the thread's roster.
    ///
    /// **Keyed on participant identity, never on the rendered handle**
    /// ([`TypedAddress::same_participant`] — see its doc for why, and for what
    /// retaining by `display()` cost here).
    /// Two Fauna members sharing a handle are two people, and the caller that
    /// picked one of them out by actor id
    /// ([`crate::ConversationsManager::evict_person_everywhere`]) must not have
    /// its precision thrown away one call later.
    ///
    /// `participant_displays` is rebuilt from what survived rather than filtered
    /// on its own: it is an index-parallel projection of `participants`
    /// (`ThreadStore::find_or_create`, [`Self::repoint_participant`]), so
    /// retaining it by value would drop a *kept* member's row whenever it
    /// happened to render the same string as the removed one — the same
    /// handle-collision bug, one field over.
    pub fn remove_participant_from(&self, id: &ThreadId, addr: &TypedAddress) {
        let mut inner = self.inner.write().unwrap();
        if let Some(detail) = inner.threads.get_mut(id) {
            detail.participants.retain(|p| !p.same_participant(addr));
            detail.participant_displays = detail.participants.iter().map(|p| p.display()).collect();
        }
    }

    /// Keep only the participants `keep` accepts; returns whether any row
    /// left. The receive-side counterpart of [`Self::remove_participant_from`]
    /// for a membership commit some OTHER member authored.
    pub fn retain_participants(&self, id: &ThreadId, keep: impl Fn(&TypedAddress) -> bool) -> bool {
        let mut inner = self.inner.write().unwrap();
        let Some(detail) = inner.threads.get_mut(id) else {
            return false;
        };
        let before = detail.participants.len();
        detail.participants.retain(|p| keep(p));
        if detail.participants.len() == before {
            return false;
        }
        detail.participant_displays = detail.participants.iter().map(|p| p.display()).collect();
        true
    }

    /// Drop a thread that never became real — a community room whose founding
    /// failed before anything was bound to it
    /// (`ConversationsManager::send_new_thread`). Only ever called on the
    /// empty thread that founding had just created: nothing user-authored
    /// rests in one, since the draft stays on the new-thread composer until
    /// the room exists.
    pub fn discard(&self, id: &ThreadId) {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        inner.threads.remove(id);
        inner.anchor_grade.remove(id);
        inner.unread.remove(id);
        inner.by_key.retain(|_, v| v != id);
        inner
            .inbound_parse
            .retain(|message_id, _| inner.by_message.get(message_id) != Some(id));
        inner
            .inbox_uid
            .retain(|message_id, _| inner.by_message.get(message_id) != Some(id));
        let inbox_uid = &inner.inbox_uid;
        inner
            .by_inbox_uid
            .retain(|_, message_id| inbox_uid.contains_key(message_id));
        inner.by_message.retain(|_, v| v != id);
    }

    pub fn rename(&self, id: &ThreadId, new_label: String) {
        let mut inner = self.inner.write().unwrap();
        if let Some(detail) = inner.threads.get_mut(id) {
            detail.label = new_label;
        }
    }

    /// Re-point a Fauna participant onto its verified successor identity —
    /// the thread-store half of rendering an identity succession as continuity
    /// (`identity-succession.md` § Propagation: consumers re-point the row
    /// rather than minting a stranger entry). In place: same list position,
    /// handle kept (the nest moved the handle to the successor inside the
    /// succession transaction). Idempotent — no participant bearing `old` is a
    /// no-op. The caller (`ConversationsManager::apply_inbound_succession`)
    /// guarantees the pair was verified.
    pub fn repoint_participant(&self, id: &ThreadId, old: &ActorId, new: ActorId) {
        let mut inner = self.inner.write().unwrap();
        // The owner's gesture named the person, and the nest moved the handle
        // to the successor inside the succession transaction, so the mark
        // moves with the row: the successor's next statement is anchored the
        // way the predecessor's was.
        if let Some(marks) = inner.anchor_grade.get_mut(id)
            && let Some(handle) = marks.remove(old)
        {
            marks.insert(new, handle);
        }
        if let Some(detail) = inner.threads.get_mut(id) {
            for i in 0..detail.participants.len() {
                let repointed = match &mut detail.participants[i] {
                    TypedAddress::Fauna { actor_id, .. } if actor_id == old => {
                        *actor_id = new;
                        true
                    }
                    _ => false,
                };
                if repointed {
                    let display = detail.participants[i].display();
                    if let Some(d) = detail.participant_displays.get_mut(i) {
                        *d = display;
                    }
                }
            }
        }
    }

    /// Name the seated participants an id-keyed handle read resolved — the
    /// thread-store half of `conversation-rooms.md` § Implementation status
    /// today's roster bullet.
    ///
    /// In place, exactly like [`Self::repoint_participant`] and for the same
    /// reason: the row already stands for this member, so the same list
    /// position must keep standing for them. Re-seating them as a newcomer
    /// would slide every later index and present as the *wrong person* on the
    /// `thread-member-chip[i]` those indices render.
    ///
    /// Only ever *adds* a name: a participant that already carries a handle is
    /// left alone, so a resolution racing the device's own richer knowledge
    /// (or an older answer arriving late) cannot overwrite it. Idempotent, and
    /// `participant_displays` follows every row it changes. Returns whether
    /// anything changed, so the caller notifies only on a real change.
    ///
    /// ⚠ Display only. `resolved` is what to *show*; nothing here touches an
    /// actor id, and every membership decision still keys on that
    /// ([`TypedAddress::same_participant`]). **And never anchor-grade**: the
    /// name is the room home's answer, so this writer leaves
    /// `Inner::anchor_grade` untouched and a succession's tier 2 never dials
    /// its domain (`identity-succession.md` § The succession statement →
    /// *which participant handles anchor tier 2*).
    pub fn name_participants(&self, id: &ThreadId, resolved: &[(ActorId, String)]) -> bool {
        let mut inner = self.inner.write().unwrap();
        let Some(detail) = inner.threads.get_mut(id) else {
            return false;
        };
        let mut changed = false;
        for i in 0..detail.participants.len() {
            let named = match &mut detail.participants[i] {
                TypedAddress::Fauna { actor_id, handle } if handle.is_empty() => resolved
                    .iter()
                    .find(|(actor, _)| actor == actor_id)
                    .map(|(_, resolved_handle)| {
                        *handle = resolved_handle.clone();
                    })
                    .is_some(),
                _ => false,
            };
            if named {
                let display = detail.participants[i].display();
                if let Some(d) = detail.participant_displays.get_mut(i) {
                    *d = display;
                }
                changed = true;
            }
        }
        changed
    }

    /// Record that **the owner's own gesture** named `actors` on thread `id` —
    /// the one provenance a succession's tier 2 may dial
    /// (`Inner::anchor_grade` carries the ruling). Marks each listed Fauna row
    /// with the handle it carries *now*; a listed actor with no row, or a row
    /// with no handle, is skipped. Idempotent.
    ///
    /// Callers are the compose and add paths only: a recipient the owner
    /// resolved and accepted, a member the owner added, and the test-only
    /// group bootstrap that stands in for them. A seat learned from the
    /// engine roster calls [`Self::inherit_anchor_grade`] instead.
    pub fn mark_anchor_grade(&self, id: &ThreadId, actors: impl IntoIterator<Item = ActorId>) {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        let Some(detail) = inner.threads.get(id) else {
            return;
        };
        for actor in actors {
            let named = detail.participants.iter().find_map(|p| match p {
                TypedAddress::Fauna { actor_id, handle }
                    if *actor_id == actor && !handle.is_empty() =>
                {
                    Some(handle.clone())
                }
                _ => None,
            });
            if let Some(handle) = named {
                inner
                    .anchor_grade
                    .entry(id.clone())
                    .or_default()
                    .insert(actor, handle);
            }
        }
    }

    /// Carry an anchor-grade mark onto thread `id`'s rows from the threads
    /// they were copied off — the provenance half of
    /// `ConversationsManager::seat_address_for`'s device-local scan. A row
    /// whose handle equals the handle the owner's gesture named for that same
    /// person on *another* thread inherits the mark; any other named row
    /// (a room-home name, a handle that differs from the one the owner typed)
    /// stays display only. Rows already marked, and nameless rows, are left
    /// alone. Idempotent.
    pub fn inherit_anchor_grade(&self, id: &ThreadId) {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        let Some(detail) = inner.threads.get(id) else {
            return;
        };
        let mut inherited: Vec<(ActorId, String)> = Vec::new();
        for participant in &detail.participants {
            let TypedAddress::Fauna { actor_id, handle } = participant else {
                continue;
            };
            if handle.is_empty()
                || inner
                    .anchor_grade
                    .get(id)
                    .is_some_and(|marks| marks.contains_key(actor_id))
            {
                continue;
            }
            let vouched_elsewhere = inner.anchor_grade.iter().any(|(other, marks)| {
                other != id
                    && marks.get(actor_id) == Some(handle)
                    && inner
                        .threads
                        .get(other)
                        .is_some_and(|d| row_carries(d, actor_id, handle))
            });
            if vouched_elsewhere {
                inherited.push((*actor_id, handle.clone()));
            }
        }
        if !inherited.is_empty() {
            inner
                .anchor_grade
                .entry(id.clone())
                .or_default()
                .extend(inherited);
        }
    }

    /// The handle the owner's own gesture named for `actor`, if this device
    /// holds one — the only participant handle a succession's tier 2 may dial
    /// (`identity-succession.md` § The succession statement → *which
    /// participant handles anchor tier 2*). A mark counts only while the live
    /// row still carries exactly that handle (see `Inner::anchor_grade`), so
    /// a re-seated row wearing a room-home name answers `None` here even
    /// though it renders by name.
    pub fn anchor_grade_handle_for(&self, actor: &ActorId) -> Option<String> {
        let inner = self.inner.read().unwrap();
        inner.anchor_grade.iter().find_map(|(id, marks)| {
            let handle = marks.get(actor)?;
            inner
                .threads
                .get(id)
                .filter(|d| row_carries(d, actor, handle))
                .map(|_| handle.clone())
        })
    }

    /// The actors thread `id` holds an anchor-grade mark for whose row still
    /// carries the marked handle — what a history slice persists
    /// ([`crate::store::history::ChannelHistorySlice::anchor_grade_handles`]).
    /// Sorted by actor bytes, so equal state encodes to equal bytes.
    pub fn anchor_grade_actors(&self, id: &ThreadId) -> Vec<ActorId> {
        let inner = self.inner.read().unwrap();
        let Some(detail) = inner.threads.get(id) else {
            return Vec::new();
        };
        let mut actors: Vec<ActorId> = inner
            .anchor_grade
            .get(id)
            .map(|marks| {
                marks
                    .iter()
                    .filter(|(actor, handle)| row_carries(detail, actor, handle))
                    .map(|(actor, _)| *actor)
                    .collect()
            })
            .unwrap_or_default();
        actors.sort_by_key(|a| a.0);
        actors
    }

    /// Re-key a thread to channel-keyed ([`ThreadKey::Channel`]). Called on the
    /// sender side once an MLS group has bootstrapped and bound its channel, so
    /// the thread is keyed by channel identity (like the receiver's
    /// `materialize_conv_thread`) — two sender-initiated groups with identical
    /// membership then stay distinct instead of colliding on the participant
    /// key from `send_new_thread`. Idempotent: a no-op once the channel key is
    /// in place. A thread carries exactly one routing key, so any prior key is
    /// dropped.
    pub fn rekey_to_channel(&self, id: &ThreadId, channel_id_hex: String) {
        let mut inner = self.inner.write().unwrap();
        if !inner.threads.contains_key(id) {
            return;
        }
        let new_key = ThreadKey::Channel {
            rail: crate::address::Rail::FaunaMls,
            channel_id_hex,
        };
        if inner.by_key.get(&new_key) == Some(id) {
            return;
        }
        inner.by_key.retain(|_, v| v != id);
        inner.by_key.insert(new_key, id.clone());
    }

    /// The hex channel id this thread is bound to, if it has been re-keyed to
    /// channel-keyed ([`ThreadKey::Channel`]) by a FaunaMls group bootstrap /
    /// Welcome materialization. `None` for participant- or subject-keyed threads
    /// (not yet bound to a nest channel). Drives the `channel_id_hex` field the
    /// e2e state protocol exposes so a real-wire test can observe the nest
    /// channel an MLS thread carries.
    /// Every thread bound to a channel — its id, the channel's hex id and its
    /// flavor — in one read of the store, so a caller filtering on flavor
    /// copies no thread it will not use (`ConversationsManager::room_post_rooms`).
    pub fn bound_threads(&self) -> Vec<(ThreadId, String, ThreadFlavor)> {
        let inner = self.inner.read().unwrap();
        inner
            .by_key
            .iter()
            .filter_map(|(k, id)| match k {
                ThreadKey::Channel { channel_id_hex, .. } => inner
                    .threads
                    .get(id)
                    .map(|d| (id.clone(), channel_id_hex.clone(), d.flavor.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn channel_hex(&self, id: &ThreadId) -> Option<String> {
        let inner = self.inner.read().unwrap();
        inner.by_key.iter().find_map(|(k, v)| match k {
            ThreadKey::Channel { channel_id_hex, .. } if v == id => Some(channel_id_hex.clone()),
            _ => None,
        })
    }

    /// Stamp <c>subject_line</c> on a specific message by id. Used by
    /// the test-helper `inject_inbound_with_subject_change_for_test`
    /// to drive subject-divider rendering in a thread whose keying
    /// subject hasn't changed.
    pub fn set_message_subject_line(
        &self,
        msg_id: &crate::message::MessageId,
        subject_line: String,
    ) {
        let mut inner = self.inner.write().unwrap();
        let Some(thread_id) = inner.by_message.get(msg_id).cloned() else {
            return;
        };
        if let Some(detail) = inner.threads.get_mut(&thread_id)
            && let Some(m) = detail.messages.iter_mut().find(|m| &m.message_id == msg_id)
        {
            m.subject_line = Some(subject_line);
        }
    }

    /// Settle a `Sent` copy against the message already held under its RFC
    /// `Message-ID` — the one rule for a Message-ID collision
    /// (`docs/goal/ui/conversations.md:1572`): a device holds one message per
    /// Message-ID, and when a `Sent` copy of it is in hand, the `Sent` copy is
    /// that message — content, ownership and delivery time. Backs
    /// [`crate::ConversationsManager::ingest_inbound_identified`]'s dedup path:
    /// self-addressed mail lands in both `INBOX` and `Sent` under the same
    /// Message-ID, and `INBOX` is polled first (`session.rs:1416-1431`), so the
    /// message is often first held with `is_own == false` — the honest answer
    /// for an `INBOX` record, which is never proof of authorship. When the
    /// `Sent` copy of the same message is ingested next, this upgrades the held
    /// copy in place rather than leaving it stranded as a receivable,
    /// spam-markable message: `is_own` flips and the message adopts the `Sent`
    /// copy's delivery timestamp, the moment the nest filed the account's
    /// submission — never the `INBOX` copy's, or whoever delivers a later
    /// identical copy would set when the account's own message shows.
    /// One-directional by construction: the caller only ever calls this when
    /// the *new* record's own `is_own` came back `true`, and nothing in this
    /// crate ever computes `is_own == true` for a record an outside party
    /// could forge (`backends::smtp::SmtpBackend::bucket_inbound` keys it on
    /// the record's `Sent`-mailbox provenance, never the `From:` header) — so
    /// there is no path back down from own to not-own to guard against, and no
    /// forged `INBOX` duplicate can ever reach this call.
    ///
    /// A Message-ID collision alone isn't enough to upgrade: `sent` and
    /// `sent_facts` are the *new* Sent record's parse, and the upgrade applies
    /// only when it matches the held message's in every field the parse fills
    /// — sender, recipients, subject, reply reference, body, body format and
    /// the attachment blocks. Otherwise a forged `INBOX` copy that merely
    /// reuses a genuine Sent Message-ID would inherit ownership of content
    /// this account never sent: other text, or the same text under another
    /// subject, to other recipients, or carrying an attacker's
    /// file. Such a held copy is a **squat** on the account's
    /// Message-ID, reported as [`SentCopyOutcome::Squat`] for the caller to
    /// displace ([`Self::evict_message`], then the ordinary ingest of the Sent
    /// copy) — so the compare decides between an in-place upgrade and a
    /// displacement, never between showing and hiding the account's message.
    /// Each field is its own `&&` arm with a pin of its own
    /// (`tests/smtp_backend_tests.rs`, the `own_upgrade_displaces_*` tests), so
    /// a dropped compare cannot hide behind the others. The delivery timestamp
    /// is the one parsed field left out of the compare: the two copies of an
    /// honest self-send are filed at different moments. A raw-bytes digest
    /// can't stand in for this compare either: the mail bridge stamps inbound
    /// self-sends with an extra `Received:` header before sealing
    /// (`libs/fauna-mail/src/received_header.rs`), so a legitimate self-send's
    /// `INBOX` and `Sent` copies routinely differ byte-for-byte while still
    /// parsing to the same message. A held message with no parse facts — one
    /// appended other than through [`Self::append_inbound_message`], such as
    /// the sending device's local echo, which is already own — is never
    /// upgraded and never displaced.
    pub fn mark_message_own(
        &self,
        sent: &MessageSnapshot,
        sent_facts: &InboundParseFacts,
    ) -> SentCopyOutcome {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        let Some(thread_id) = inner.by_message.get(&sent.message_id) else {
            return SentCopyOutcome::NotHeld;
        };
        let Some(m) = inner.threads.get_mut(thread_id).and_then(|detail| {
            detail
                .messages
                .iter_mut()
                .find(|m| m.message_id == sent.message_id)
        }) else {
            return SentCopyOutcome::NotHeld;
        };
        if m.is_own {
            return SentCopyOutcome::AlreadyOwn;
        }
        let Some(held_facts) = inner.inbound_parse.get(&sent.message_id) else {
            return SentCopyOutcome::NotComparable;
        };
        if m.sender == sent.sender
            && held_facts.recipients == sent_facts.recipients
            && held_facts.subject == sent_facts.subject
            && m.reply_to == sent.reply_to
            && m.body == sent.body
            && held_facts.body_format == sent_facts.body_format
            && attachment_blocks(&m.document) == attachment_blocks(&sent.document)
        {
            m.is_own = true;
            m.timestamp_ms = sent.timestamp_ms;
            SentCopyOutcome::Upgraded
        } else {
            SentCopyOutcome::Squat
        }
    }

    /// Remove one held message from its thread — the displacement half of the
    /// Message-ID collision rule (`docs/goal/ui/conversations.md:1572`): a
    /// held copy that [`Self::mark_message_own`] reported as a
    /// [`SentCopyOutcome::Squat`] leaves, and the `Sent` copy then ingests
    /// afresh through the ordinary path, threading by its own key. Drops the
    /// message, its parse facts and its id mapping; clears the thread's
    /// selection if it pointed at the message; leaves the thread itself in
    /// place, reporting whether it is now empty so the caller can
    /// [`Self::discard`] one that binds nothing else. `None` when no message
    /// is held under `msg_id`. Never called for an own message: the rule only
    /// ever displaces a not-own copy with a `Sent` copy of the same id.
    pub fn evict_message(&self, msg_id: &MessageId) -> Option<EvictedMessage> {
        let mut inner = self.inner.write().unwrap();
        let inner = &mut *inner;
        let thread_id = inner.by_message.remove(msg_id)?;
        inner.inbound_parse.remove(msg_id);
        inner.forget_inbox_uid(msg_id);
        let detail = inner.threads.get_mut(&thread_id)?;
        detail.messages.retain(|m| &m.message_id != msg_id);
        if detail.selected_message_id.as_ref() == Some(msg_id) {
            detail.selected_message_id = None;
        }
        Some(EvictedMessage {
            emptied: detail.messages.is_empty(),
            thread_id,
        })
    }

    /// Force a loaded message's `is_own` flag — test-only, backing
    /// [`crate::ConversationsManager::inject_own_for_test`] so a client e2e can
    /// materialise an own bubble without a live send.
    ///
    /// Gated like the manager-level seam it backs (`testing.md` § convention 15).
    /// It is not `uniffi::export`ed, so it was never part of the *exported*
    /// automation surface — but ungated it compiled into every release build of
    /// this crate (nest, linux, tui and all three native FFI artifacts), which is
    /// the same dead-test-code class the shared-Rust leg closed elsewhere. Its
    /// only caller is the gated `inject_own_for_test`.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_message_is_own_for_test(&self, msg_id: &crate::message::MessageId, is_own: bool) {
        let mut inner = self.inner.write().unwrap();
        let Some(thread_id) = inner.by_message.get(msg_id).cloned() else {
            return;
        };
        if let Some(detail) = inner.threads.get_mut(&thread_id)
            && let Some(m) = detail.messages.iter_mut().find(|m| &m.message_id == msg_id)
        {
            m.is_own = is_own;
        }
    }

    /// Force a loaded message's content [`labels`](crate::message::MessageSnapshot::labels)
    /// — test-only, backing [`crate::ConversationsManager::inject_inbound_with_labels_for_test`].
    /// The generic inject path (`ingest_inbound`) does not classify (only the real
    /// MLS receive path's `observe_local_detection` does), so a client e2e that
    /// needs a *labeled* bubble — the family-safety content-floor render, the
    /// content-label badge — stages the labels directly here, exactly as the feed
    /// stages `TestPostSpec.labels`.
    ///
    /// Gated for the same reason as [`Self::set_message_is_own_for_test`] above.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    pub fn set_message_labels_for_test(
        &self,
        msg_id: &crate::message::MessageId,
        labels: Vec<fauna_core::content_category::ContentLabelEntry>,
    ) {
        let mut inner = self.inner.write().unwrap();
        let Some(thread_id) = inner.by_message.get(msg_id).cloned() else {
            return;
        };
        if let Some(detail) = inner.threads.get_mut(&thread_id)
            && let Some(m) = detail.messages.iter_mut().find(|m| &m.message_id == msg_id)
        {
            m.labels = labels;
        }
    }

    /// Wipe all thread state. Test-only helper used by e2e between-test
    /// resets; preserves no history.
    pub fn clear(&self) {
        let mut inner = self.inner.write().unwrap();
        inner.threads.clear();
        inner.anchor_grade.clear();
        inner.by_key.clear();
        inner.by_message.clear();
        inner.inbound_parse.clear();
        inner.inbox_uid.clear();
        inner.by_inbox_uid.clear();
        inner.unread.clear();
        // Read positions are the account's: the next identity's arrive from its
        // own store, and until they do its native messages meet the floor.
        inner.read_positions.clear();
        inner.positions_known = false;
        // The next identity's run starts now: what its catch-up replays is its
        // history, not news.
        inner.launch_floor_ms = launch_floor_ms();
        inner.next_id = 0;
    }
}

/// The most of a latest message's `body` the thread-list snippet is derived from. A
/// payload bound, not a visual one: every app clamps the row to one line, so the
/// preview never runs out of text, while the parse stays constant-cost however large
/// the body (`conversations.md` § State & data shape).
pub(crate) const SNIPPET_PARSE_MAX_BYTES: usize = 4096;

/// The most bytes the thread-list snippet itself carries — into every
/// `ThreadSummary`, the e2e state row, and a desktop notification body.
pub(crate) const SNIPPET_MAX_BYTES: usize = 512;

/// Whether `detail` seats `actor` under exactly `handle` — the liveness test
/// every `Inner::anchor_grade` read applies before honouring a mark.
fn row_carries(detail: &ThreadDetail, actor: &ActorId, handle: &str) -> bool {
    detail.participants.iter().any(|p| {
        matches!(p, TypedAddress::Fauna { actor_id, handle: h } if actor_id == actor && h == handle)
    })
}

/// The floor a fresh (or freshly wiped) store starts its run at.
fn launch_floor_ms() -> i64 {
    fauna_core::data::Timestamp::now_millis_or_zero() as i64
}

fn summarize(detail: &ThreadDetail, unread: Option<&HashSet<MessageId>>) -> ThreadSummary {
    let last = detail.messages.last();
    // Counted against the live messages rather than taken as the set's size: a
    // message that was evicted, deleted, or turned out to be the user's own
    // (`ThreadStore::mark_message_own`) stops counting without any of those
    // paths knowing the set exists.
    let unread_count = unread.map_or(0, |ids| {
        detail
            .messages
            .iter()
            .filter(|m| !m.is_own && !m.deleted && ids.contains(&m.message_id))
            .count() as u32
    });
    ThreadSummary {
        thread_id: detail.thread_id.clone(),
        rail: detail.rail,
        glyph: detail.rail.glyph(),
        flavor: detail.flavor.clone(),
        label: detail.label.clone(),
        snippet: last.map(|m| snippet_preview(&m.body)).unwrap_or_default(),
        last_activity_ms: last.map(|m| m.timestamp_ms).unwrap_or(0),
        unread_count,
        participant_count: detail.participants.len() as u32,
        // Projected by `ConversationsManager::snapshot` from the rail's
        // registry, never stored.
        bridge: None,
        guardian_state: None,
    }
}

/// The thread-list snippet: a plaintext preview, not rendered markup — Markdown markers
/// stripped via the shared `fauna_core` helper so a `**bold**` body previews as `bold`
/// on every app (the detail bubble still renders the formatted body) — and a BOUNDED one.
///
/// Unbounded, a multi-megabyte mail made the snippet the whole body: every `snapshot()`
/// re-parsed it (~270 ms per call in a debug build for ~3 MiB), every state push shipped
/// it, and windows laid it out in a one-line list row, which was most of the time that
/// mail took to open (`mail-message-size.md` § Implementation status today). So only a
/// byte-bounded prefix is parsed, and the result is capped — cut back to the last
/// whitespace when there is one, so a preview never ends mid-word.
pub(crate) fn snippet_preview(body: &str) -> String {
    use fauna_core::encoding::truncate_to_char_boundary;
    let prefix = truncate_to_char_boundary(body, SNIPPET_PARSE_MAX_BYTES);
    let plain = fauna_core::markdown::markdown_to_plaintext(prefix);
    if plain.len() <= SNIPPET_MAX_BYTES {
        return plain;
    }
    let cut = truncate_to_char_boundary(&plain, SNIPPET_MAX_BYTES);
    match cut.rfind(char::is_whitespace) {
        Some(i) if i > 0 => cut[..i].to_string(),
        _ => cut.to_string(),
    }
}

/// Whether a thread's latest message, as FULL plaintext, contains `needle_lower`
/// (already lowercased). The list filter matches this rather than the bounded
/// [`snippet_preview`], so a search term past what a row shows still finds its thread
/// (`conversations.md` § Where logic lives → *Thread-list sort + search filtering*).
/// Only paid while a query is active.
fn latest_plaintext_contains(detail: &ThreadDetail, needle_lower: &str) -> bool {
    detail.messages.last().is_some_and(|m| {
        fauna_core::markdown::markdown_to_plaintext(&m.body)
            .to_lowercase()
            .contains(needle_lower)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::{Rail, TypedAddress};
    use crate::message::{BodyFormat, MessageBadges, MessageId, MessageSnapshot};

    fn email(addr: &str) -> TypedAddress {
        TypedAddress::Email {
            email_address: addr.to_string(),
        }
    }

    fn msg(id: &str, body: &str) -> MessageSnapshot {
        MessageSnapshot {
            message_id: MessageId(id.to_string()),
            sender: email("alice@example.com"),
            sender_display: String::new(),
            body: body.to_string(),
            document: crate::message::document_for_message(body, BodyFormat::Markdown, &[]),
            timestamp_ms: 1,
            subject_line: None,
            badges: MessageBadges::default(),
            reply_to: None,
            reactions: vec![],
            deleted: false,
            is_own: false,
            legal_takedown_ref: None,
            labels: vec![],
            plane_ref: None,
            can_delete: false,
        }
    }

    fn fauna(handle: &str, seed: u8) -> TypedAddress {
        TypedAddress::Fauna {
            handle: handle.to_string(),
            actor_id: ActorId([seed; 32]),
        }
    }

    fn channel_thread(store: &ThreadStore, hex: &str, parts: Vec<TypedAddress>) -> ThreadId {
        store.find_or_create(
            ThreadKey::Channel {
                rail: Rail::FaunaMls,
                channel_id_hex: hex.to_string(),
            },
            parts,
            ThreadFlavor::MlsGroup,
            None,
        )
    }

    // ── handle provenance: which rows a succession's tier 2 may dial ────────
    //
    // `identity-succession.md` § The succession statement → *which participant
    // handles anchor tier 2*. `Inner::anchor_grade` owns the shape; these pin
    // its writers and its one read.

    /// A room-home name (`name_participants`) renders on the row and is never
    /// anchor-grade; the owner's gesture (`mark_anchor_grade`) is.
    #[test]
    fn a_roster_name_renders_but_never_anchors() {
        let store = ThreadStore::new();
        let alice = ActorId([1; 32]);
        let id = channel_thread(&store, "c0ffee", vec![fauna("", 1)]);

        assert!(store.name_participants(&id, &[(alice, "alice@host.example".into())]));
        assert_eq!(
            store.get(&id).unwrap().participant_displays,
            vec!["alice@host.example".to_string()],
            "the name renders"
        );
        assert_eq!(
            store.anchor_grade_handle_for(&alice),
            None,
            "and never anchors"
        );
        assert!(store.anchor_grade_actors(&id).is_empty());

        // The owner's gesture on ANOTHER thread for the same person, same
        // handle, vouches for it there — and only there until inherited.
        let typed = channel_thread(&store, "beef", vec![fauna("alice@host.example", 1)]);
        store.mark_anchor_grade(&typed, [alice]);
        assert_eq!(
            store.anchor_grade_handle_for(&alice).as_deref(),
            Some("alice@host.example")
        );
        assert_eq!(store.anchor_grade_actors(&typed), vec![alice]);
    }

    /// A seat copied off an owner-typed row inherits the mark; one copied off
    /// a room-home name, or carrying a different string, does not.
    #[test]
    fn a_seat_inherits_anchor_grade_only_from_an_owner_typed_row_with_the_same_handle() {
        let store = ThreadStore::new();
        let alice = ActorId([1; 32]);
        let bob = ActorId([2; 32]);
        let typed = channel_thread(&store, "beef", vec![fauna("alice@home.example", 1)]);
        store.mark_anchor_grade(&typed, [alice]);
        let named = channel_thread(&store, "cafe", vec![fauna("", 2)]);
        store.name_participants(&named, &[(bob, "bob@host.example".into())]);

        // The Welcome seat: alice's row copied her owner-typed name, bob's
        // copied his room-home name, and a third row for alice carries a
        // string the owner never typed.
        let welcome = channel_thread(
            &store,
            "c0ffee",
            vec![fauna("alice@home.example", 1), fauna("bob@host.example", 2)],
        );
        store.inherit_anchor_grade(&welcome);
        assert_eq!(store.anchor_grade_actors(&welcome), vec![alice]);

        let other = channel_thread(&store, "d00d", vec![fauna("alice@elsewhere.example", 1)]);
        store.inherit_anchor_grade(&other);
        assert!(
            store.anchor_grade_actors(&other).is_empty(),
            "a different string is not the handle the owner vouched for"
        );
    }

    /// A verified succession re-point carries the mark to the successor — the
    /// nest moved the handle inside the succession transaction, and the
    /// owner's gesture named the person, not the key.
    #[test]
    fn a_repoint_carries_the_mark_to_the_successor() {
        let store = ThreadStore::new();
        let alice = ActorId([1; 32]);
        let alice2 = ActorId([9; 32]);
        let id = channel_thread(&store, "beef", vec![fauna("alice@home.example", 1)]);
        store.mark_anchor_grade(&id, [alice]);

        store.repoint_participant(&id, &alice, alice2);
        assert_eq!(store.anchor_grade_handle_for(&alice), None);
        assert_eq!(
            store.anchor_grade_handle_for(&alice2).as_deref(),
            Some("alice@home.example")
        );
    }

    /// A mark is honoured only while the live row carries exactly the marked
    /// handle: a member removed and re-seated under a room-home name reads
    /// display-only even though a stale mark exists.
    #[test]
    fn a_mark_is_honoured_only_while_the_row_carries_the_marked_handle() {
        let store = ThreadStore::new();
        let alice = ActorId([1; 32]);
        let id = channel_thread(&store, "beef", vec![fauna("alice@home.example", 1)]);
        store.mark_anchor_grade(&id, [alice]);

        store.remove_participant_from(&id, &fauna("alice@home.example", 1));
        assert_eq!(store.anchor_grade_handle_for(&alice), None);
        assert!(store.anchor_grade_actors(&id).is_empty());

        // Re-seated nameless, then named by the room home under another
        // domain: the stale mark does not vouch for the new string.
        store.add_participant_to(&id, fauna("", 1));
        store.name_participants(&id, &[(alice, "alice@host.example".into())]);
        assert_eq!(store.anchor_grade_handle_for(&alice), None);

        // But the optimistic remove's rollback — same row, same handle — is
        // vouched for again by the mark it never swept.
        store.remove_participant_from(&id, &fauna("alice@host.example", 1));
        store.add_participant_to(&id, fauna("alice@home.example", 1));
        assert_eq!(
            store.anchor_grade_handle_for(&alice).as_deref(),
            Some("alice@home.example")
        );
    }

    /// The list snippet strips Markdown to plaintext (`**bold**` → `bold`) so a
    /// row never shows raw source. Regression for the linux UI-review finding.
    #[test]
    fn snippet_is_markdown_stripped_plaintext() {
        let store = ThreadStore::new();
        let parts = vec![email("alice@example.com"), email("me@example.com")];
        let id = store.find_or_create(
            ThreadKey::Participants {
                rail: Rail::Smtp,
                participants: parts.clone(),
            },
            parts,
            ThreadFlavor::OneToOne,
            None,
        );
        store.append_message(&id, msg("m1", "**bold** preview"));

        let summaries = store.list_summaries();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].snippet, "bold preview");
    }

    /// The snippet always reflects the *last* message (latest activity).
    #[test]
    fn snippet_tracks_last_message() {
        let store = ThreadStore::new();
        let parts = vec![email("alice@example.com")];
        let id = store.find_or_create(
            ThreadKey::Participants {
                rail: Rail::Smtp,
                participants: parts.clone(),
            },
            parts,
            ThreadFlavor::OneToOne,
            None,
        );
        store.append_message(&id, msg("m1", "first"));
        store.append_message(&id, msg("m2", "see [docs](https://example.com)"));

        assert_eq!(store.list_summaries()[0].snippet, "see docs");
    }

    /// A huge latest message must not make every summary carry — and every
    /// `snapshot()` re-derive — the whole body: the snippet is a bounded preview of
    /// the body's beginning. Measured on windows (`mail-message-size.md`
    /// § Implementation status today): a ~3 MiB mail made the snippet 3.1 MB,
    /// re-parsed on every snapshot and laid out in a one-line list row.
    #[test]
    fn snippet_is_a_bounded_preview_of_a_huge_body() {
        let store = ThreadStore::new();
        let parts = vec![email("alice@example.com")];
        let id = store.find_or_create(
            ThreadKey::Participants {
                rail: Rail::Smtp,
                participants: parts.clone(),
            },
            parts,
            ThreadFlavor::OneToOne,
            None,
        );
        let line = format!("{}\n", "z".repeat(76));
        let body = format!("HEAD\n{}TAIL\n", line.repeat(40_000));
        store.append_message(&id, msg("m1", &body));

        let snippet = &store.list_summaries()[0].snippet;
        assert!(
            snippet.len() <= 1024,
            "the snippet must be a bounded preview; got {} bytes",
            snippet.len()
        );
        assert!(
            snippet.starts_with("HEAD"),
            "the preview is the beginning of the body; got {:?}",
            &snippet[..snippet.len().min(40)]
        );
    }

    /// A thread must never show the same `message_id` twice, no matter how many
    /// append paths reach the store (the user-reported mail-duplication invariant).
    /// The id-level dedup in `ConversationsManager::ingest_inbound` covers the mail
    /// rail, but `ingest_inbound_to_thread` (FaunaMls) and any future caller append
    /// without it — so the store itself is the last line of defense.
    #[test]
    fn append_message_dedups_a_repeated_message_id() {
        let store = ThreadStore::new();
        let parts = vec![email("alice@example.com")];
        let id = store.find_or_create(
            ThreadKey::Participants {
                rail: Rail::Smtp,
                participants: parts.clone(),
            },
            parts,
            ThreadFlavor::OneToOne,
            None,
        );
        store.append_message(&id, msg("dup-1", "first arrival"));
        // A second append with the SAME id (e.g. a no-dedup path, a re-poll, or a
        // cross-feed copy) must NOT grow the thread.
        store.append_message(&id, msg("dup-1", "duplicate copy"));

        let detail = store.get(&id).expect("thread exists");
        assert_eq!(
            detail.messages.len(),
            1,
            "a repeated message_id appends exactly once"
        );
        assert_eq!(
            detail.messages[0].body, "first arrival",
            "the first copy stands; the duplicate is dropped"
        );
    }

    /// `message_body` returns the retained plaintext by message id (the
    /// moderation train-correction text source) and `None` for an unknown id.
    #[test]
    fn message_body_looks_up_retained_plaintext() {
        let store = ThreadStore::new();
        let parts = vec![email("alice@example.com")];
        let id = store.find_or_create(
            ThreadKey::Participants {
                rail: Rail::Smtp,
                participants: parts.clone(),
            },
            parts,
            ThreadFlavor::OneToOne,
            None,
        );
        store.append_message(&id, msg("m1", "buy cheap pills now"));

        assert_eq!(
            store.message_body(&crate::message::MessageId("m1".into())),
            Some("buy cheap pills now".to_string())
        );
        assert_eq!(
            store.message_body(&crate::message::MessageId("absent".into())),
            None
        );
    }
}
